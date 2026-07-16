//! Passphrase resolution for the two operation classes (spec A0):
//! operational commands read env → OS keychain → TTY prompt; admin commands
//! (key export, decrypt, policy edits) accept ONLY a live TTY prompt, so an
//! agent that never learned the passphrase cannot perform them.

use eyre::{Result, eyre};
use zeroize::Zeroizing;

const KEYRING_SERVICE: &str = "rwa";

/// `RWA_KEYRING_DISABLE=1` removes the keychain step entirely (tests, headless CI).
pub(crate) fn keyring_disabled() -> bool {
    std::env::var("RWA_KEYRING_DISABLE").is_ok_and(|v| v == "1")
}

/// Operational chain core, dependency-injected for unit tests.
fn resolve_chain(
    env: Option<String>,
    keychain: impl FnOnce() -> Option<Zeroizing<String>>,
    prompt: impl FnOnce() -> Result<Zeroizing<String>>,
) -> Result<Zeroizing<String>> {
    if let Some(p) = env {
        return Ok(Zeroizing::new(p));
    }
    if let Some(p) = keychain() {
        return Ok(p);
    }
    prompt()
}

/// Passphrase for operational commands (trading, `keys show`, policy reads):
/// `RWA_PASSPHRASE` → OS keychain (per wallet name) → interactive prompt.
pub(crate) fn operational_passphrase(wallet_name: &str) -> Result<Zeroizing<String>> {
    let env = std::env::var("RWA_PASSPHRASE").ok();
    if env.is_some() {
        crate::wallets::warn_passphrase_env_once();
    }
    let name = wallet_name.to_string();
    resolve_chain(env, move || keyring_get(&name), tty_prompt)
}

/// Passphrase for ADMIN commands: a live TTY prompt or nothing. Env and
/// keychain are deliberately not consulted — see the spec's security invariant.
pub(crate) fn admin_passphrase(what: &str) -> Result<Zeroizing<String>> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        return Err(rwa_ondo::usecases::gm::GmTradeError::new(
            rwa_ondo::usecases::gm::GmTradeErrorKind::InteractiveRequired,
            format!(
                "{what} requires typing the wallet passphrase at a terminal; \
                 RWA_PASSPHRASE and the OS keychain are deliberately not consulted for it"
            ),
        )
        .into());
    }
    tty_prompt()
}

fn tty_prompt() -> Result<Zeroizing<String>> {
    rpassword::prompt_password("Wallet passphrase: ")
        .map(Zeroizing::new)
        .map_err(|e| {
            eyre!("Failed to read passphrase ({e}). No terminal to prompt? Set RWA_PASSPHRASE or run interactively.")
        })
}

fn keyring_get(wallet: &str) -> Option<Zeroizing<String>> {
    if keyring_disabled() {
        return None;
    }
    match keyring::Entry::new(KEYRING_SERVICE, wallet).and_then(|e| e.get_password()) {
        Ok(p) => Some(Zeroizing::new(p)),
        Err(e) => {
            if std::env::var("RWA_DEBUG").is_ok_and(|v| v == "1") {
                eprintln!("note: keychain lookup for wallet '{wallet}' failed: {e}");
            }
            None
        }
    }
}

/// Store the passphrase for `wallet`. Errors loudly when the keychain is
/// disabled/unavailable — storage is an explicit user request, not best-effort.
pub(crate) fn keyring_set(wallet: &str, pass: &str) -> Result<()> {
    if keyring_disabled() {
        return Err(eyre!("OS keychain is disabled (RWA_KEYRING_DISABLE=1) — cannot store the passphrase"));
    }
    keyring::Entry::new(KEYRING_SERVICE, wallet)
        .and_then(|e| e.set_password(pass))
        .map_err(|e| eyre!("OS keychain unavailable: {e}"))
}

/// Remove the stored passphrase. Ok(false) when nothing was stored (idempotent).
pub(crate) fn keyring_delete(wallet: &str) -> Result<bool> {
    if keyring_disabled() {
        return Err(eyre!("OS keychain is disabled (RWA_KEYRING_DISABLE=1) — cannot remove the passphrase"));
    }
    let entry = keyring::Entry::new(KEYRING_SERVICE, wallet).map_err(|e| eyre!("OS keychain unavailable: {e}"))?;
    match entry.delete_credential() {
        Ok(()) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(e) => Err(eyre!("OS keychain unavailable: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroize::Zeroizing;

    fn z(s: &str) -> Zeroizing<String> {
        Zeroizing::new(s.to_string())
    }

    // env wins over keychain and prompt.
    #[test]
    fn chain_env_wins() {
        let got = resolve_chain(
            Some("from-env".into()),
            || panic!("keychain must not be consulted when env is set"),
            || panic!("prompt must not run when env is set"),
        )
        .unwrap();
        assert_eq!(&**got, "from-env");
    }

    // keychain wins over prompt when env is absent.
    #[test]
    fn chain_keychain_wins_over_prompt() {
        let got = resolve_chain(None, || Some(z("from-keychain")), || {
            panic!("prompt must not run when keychain answered")
        })
        .unwrap();
        assert_eq!(&**got, "from-keychain");
    }

    // both absent → prompt runs.
    #[test]
    fn chain_falls_back_to_prompt() {
        let got = resolve_chain(None, || None, || Ok(z("typed"))).unwrap();
        assert_eq!(&**got, "typed");
    }

    // empty env value is still "set" (matches today's RWA_PASSPHRASE semantics).
    #[test]
    fn chain_empty_env_is_still_env() {
        let got = resolve_chain(Some(String::new()), || panic!("no keychain"), || panic!("no prompt")).unwrap();
        assert_eq!(&**got, "");
    }
}
