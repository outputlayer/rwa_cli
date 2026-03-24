use alloy_primitives::Address;
use eyre::Result;
use serde::Deserialize;
use std::collections::HashMap;
use tracing::warn;

const TOKENLIST_URL: &str = "https://raw.githubusercontent.com/ondoprotocol/ondo-global-markets-token-list/main/tokenlist.json";

/// A GM token entry with addresses on Ethereum, BNB Chain and Solana.
#[derive(Debug, Clone)]
pub struct GmTokenEntry {
    pub symbol: String,
    pub name: String,
    pub decimals: u8,
    pub bsc_address: Option<Address>,
    pub eth_address: Option<Address>,
    pub solana_address: Option<String>,
}

#[derive(Deserialize)]
struct TokenListJson {
    tokens: Vec<TokenListItem>,
}

#[derive(Deserialize)]
struct TokenListItem {
    #[serde(rename = "chainId")]
    chain_id: u64,
    address: String,
    name: String,
    symbol: String,
    decimals: u8,
}

/// Fetch the GM token list from GitHub, falling back to the static list on error.
pub async fn get_token_list() -> Vec<GmTokenEntry> {
    match fetch_token_list().await {
        Ok(tokens) => {
            tracing::info!("Loaded {} tokens from GitHub tokenlist", tokens.len());
            tokens
        }
        Err(e) => {
            warn!("Failed to fetch token list from GitHub, using static fallback: {e}");
            static_token_list()
        }
    }
}

/// Fetch and parse the Ondo tokenlist from GitHub.
pub async fn fetch_token_list() -> Result<Vec<GmTokenEntry>> {
    let text = reqwest::Client::new()
        .get(TOKENLIST_URL)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;

    let list: TokenListJson = serde_json::from_str(&text)?;
    Ok(group_tokens(list.tokens))
}

fn group_tokens(items: Vec<TokenListItem>) -> Vec<GmTokenEntry> {
    // Build a lookup of Solana addresses from the static list
    let sol_lookup: HashMap<&str, &str> = GM_TOKENS_STATIC
        .iter()
        .map(|&(sym, _, sol)| (sym, sol))
        .collect();

    let mut map: HashMap<String, GmTokenEntry> = HashMap::new();

    for item in items {
        let entry = map.entry(item.symbol.clone()).or_insert_with(|| GmTokenEntry {
            symbol: item.symbol.clone(),
            name: item.name.clone(),
            decimals: item.decimals,
            bsc_address: None,
            eth_address: None,
            solana_address: sol_lookup.get(item.symbol.as_str()).map(|s| s.to_string()),
        });

        if let Ok(addr) = item.address.parse::<Address>() {
            match item.chain_id {
                56 => entry.bsc_address = Some(addr),
                1 => entry.eth_address = Some(addr),
                _ => {}
            }
        }
    }

    let mut tokens: Vec<_> = map.into_values().collect();
    tokens.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    tokens
}

/// Convert the static fallback list to `GmTokenEntry` entries.
pub fn static_token_list() -> Vec<GmTokenEntry> {
    GM_TOKENS_STATIC
        .iter()
        .map(|&(symbol, bsc, sol)| GmTokenEntry {
            symbol: symbol.to_string(),
            name: String::new(),
            decimals: 18,
            bsc_address: bsc.parse().ok(),
            eth_address: None,
            solana_address: if sol.is_empty() { None } else { Some(sol.to_string()) },
        })
        .collect()
}

/// Static fallback list of Ondo GM tokens (264 tokens).
/// Tuple: (symbol, bsc_address, solana_mint_address)
static GM_TOKENS_STATIC: &[(&str, &str, &str)] = &[
    ("AALon", "0x02d608506ca0048d0d991a11f1e7fb8cad1e44f8", "9wYZetvT8J2ptfsRca5gzLBGvcUug38mp9yT3xaondo"),
    ("AAPLon", "0x390a684ef9cade28a7ad0dfa61ab1eb3842618c4", "123mYEnRLM2LLYsJW3K6oyYh8uP1fngj732iG638ondo"),
    ("ABBVon", "0x8677abad7b458bf16a0fb2676dfc7d3f55ac202a", "MFerpBVGKZh2jXN7cbJdXRXQTp6j6pbSnSZrfWrondo"),
    ("ABNBon", "0xef80743f78d98fc2b47a2253b293152ce8b879ba", "128qNYovdGv2YqayErcJgU7gDwbNVX1VuoxbtWz8ondo"),
    ("ABTon", "0x5a20886b575058dd7299785f0ea9b1172942a3e0", "129gRoHKhVg7CvPMrqVsEB4uYZo6zV4yDZX6NBg9ondo"),
    ("ACHRon", "0x91c62325f901ee29da8e521cfe68980332a4ca06", "KcCVQxG9LhFYP5o9DWFKTFgFShPPQkDEemVbiFyondo"),
    ("ACNon", "0x7af44d51d1fb88c5b74fc71d3cba649bb8099d14", "12LxMMJYVSf4LoeqjFE47BQQNRciaH9E3nbDfjH4ondo"),
    ("ADBEon", "0xcb22db0ecb6fe58b7b47db443dcfdfdfbf729cef", "12Rh6JhfW4X5fKP16bbUdb4pcVCKDHFB48x8GG33ondo"),
    ("ADIon", "0x0e246e05212dbbd78a354c072a92b4e5723b2fa0", "LmTMwmZLNZszn3qpjmnbhfP12U4qWDivaEBwSBSondo"),
    ("AGGon", "0x08ce97f3d5cf11e577d091ab048bc5e2eae3fabb", "13qTjKx53y6LKGGStiKeieGbnVx3fx1bbwopKFb3ondo"),
    ("ALBon", "0x0B790ABd6594918DE1022233b7cc79baDB84d92a", "B5KufqHkskgGYwMXtL8FSHgREAkMQvE3ykhH5Kmondo"),
    ("AMATon", "0x5ecc352c4640f1d26bd231dbbd171f40f7d0eec6", "7eRX747PSbVtGVx3qD5UFdkNM2BfTy86ikUiCMhondo"),
    ("AMCon", "0x1d7b5e06fdbe4fd33f5c64c081e32b5d539751d0", "C9xNaNujcF1a5fidWAAFReFYqhLRVbyk4yPyGqzondo"),
    ("AMDon", "0x9f16e46c73b43bdb70861247d537bee4ea18f639", "14diAn5z8kjrKwSC8WLqvBqqe5YmihJhjxRxd8Z6ondo"),
    ("AMGNon", "0xfbdf0366f800cc79d6663da26bc0bf21fb455aa6", "SS6AEWhzRrxhL2cXzKKjhFt3rCzmHHGKmFyugDTondo"),
    ("AMZNon", "0x4553cfe1c09f37f38b12dc509f676964e392f8fc", "14Tqdo8V1FhzKsE3W2pFsZCzYPQxxupXRcqw9jv6ondo"),
    ("ANETon", "0x538e2838f9ebc9b891399df4a8dcc42890d9dc20", "Cq6QtvHpXbJWtFaiMhUDtHy8YVZ95gcD1oZ1cohondo"),
    ("APLDon", "0x18De24acb876C0B8392d9C55583Bb21c0355980b", "B6WqvLGXdGqpw7qgxeb5EGiRZEYo2apWpQybjYuondo"),
    ("APOon", "0x5630b5741a33371d9d935283849a16dc808f7f3a", "14VXAhoa1R74vi1ZuiQyGLJrnDMfoFBPJSCpGVz3ondo"),
    ("APPon", "0xedb3124e96c64c177eb709cbc64f9977db40ea74", "14Z8rQQe2Aza33YgEUmj3g3QGNz8DXLiFPuCnsD1ondo"),
    ("ARMon", "0x527c6436e1eaa4f2065cde4090f798cb5d031dd6", "15SsCZqCsM9fZGhTmP4rdJTPT9WGZKazDSsgeQ8ondo"),
    ("ASMLon", "0xb034f6cb52b7f2fd5a7eeeffca6b9adcd6b9a6f6", "1eLZPRsn8bAKmoxsqDMH9Q2m2k7GMNp6RLSQGm8ondo"),
    ("ASTSon", "0x45Abf29515bc23F8c0Ed2a06584444cE473A75FB", "B6ry9goGNvVbhq7gWHzs3p6emJ1gLaMhu4By9TTondo"),
    ("AVGOon", "0x0ed2e3180edf393e6bf8db124bd15ddd54de150a", "1FWZtdWN7y38BSXGzbs8D6Shk88oL9atDNgbVz9ondo"),
    ("AXPon", "0xd803f8777187d6dee1ea57854aeb957043fb1675", "1WxT6NdK7uqpfXuKpALxL2n3f7Rq61XXeHA8UM4ondo"),
    ("BABAon", "0xd5964f3fcee8d649995ab88f04b8982539c282d2", "1zvb9ELBFShBCWKEk5jRTJAaPAwtVt7quEXx1X4ondo"),
    ("BACon", "0xd615468088b19fb9d4f03cb3ce9e33876ff3db99", "Wk8gC6iTNp8dqd4ghkJ3h1giiUnyhykwHh7tYWjondo"),
    ("BAon", "0xf21132a811ad1a878e21af60f64d4e690c9daa42", "1YVZ4LGpq8CAhpdpm3mgy7GgPb83gJczCpxLUQ3ondo"),
    ("BBAIon", "0x7ba995f1662a01f3be0dc299ce94bb7e9c7075f5", "YXE7mph6XhsgnyezkMEcTuohSuWhbLWfwx2Hh6mondo"),
    ("BIDUon", "0x467e59ce5d5fe01686d4a80dd1e1dae13549aa6c", "54CoRF2FYMZNJg9tS36xq5BUcLZ7rju1r59jGc2ondo"),
    ("BILIon", "0x91fc7371d6de682a1e8cfcb4eb7da693312a03a4", "14kLsQVmc64qZexYuR4XGop9y8BeMkd77pJUm1Rhondo"),
    ("BINCon", "0x940f442746d9ae699e63c378d52c4494ea02684f", "mhZ69E1vDnAsQJXAwarLYSX5tmgeMajXBJ2rXAcondo"),
    ("BLKon", "0x24f5471183ea549987f245d6ce236b6108869c92", "5H1VpMzRuoNtRbPTRCz35ETtEUtnkt8hJuQb9v7ondo"),
    ("BLSHon", "0xfbe22d27b6e153244882fd7bdfe7c6109918281b", "A9PFmw9Hu8zzxDUoU351pio1E1XWBWBfWnjT9qoondo"),
    ("BMNRon", "0x52ad57a7ea642e99a892afc79e937b383f1b59e9", "MYXqkDYbzr7vjXAz2BapR4AiYRXzoikGirrLoRzondo"),
    ("BNOon", "0x5f2d37192576a6804F44722eB828E280D5FB43Dc", "BAU83kqEqhyiexfAMQhZZE5KnGogSqh17fJc44Sondo"),
    ("BTGOon", "0x5fA699c0c1319b8D86489AF77dFDe4Fa97B47DF8", "bgJWGuQxyoyFeXwzYZKBmoujVdatGFYPNFnv1a6ondo"),
    ("BTGon", "0xe2ac868f2fd097086d83bc939248e5ae08d35da4", "cBnVXDyZgaaLZM18wAmqsUKnRUFAEJWbq6VuUoaondo"),
    ("BZon", "0xc2c7fcddc37f6737ca2481ebda6b81ee279fe20c", "doPqjCxi6UkANkvMz5fSuYGEo5PGppVpTZMeB5vondo"),
    ("CAPRon", "0x812Fc2943371C952c6c8Daf99Fe665Eb0e40Cd27", "BS8zoc6pmALQnBhBDFak6eFhgGHjpebnHzsxApgondo"),
    ("CATon", "0x274b0cb6db9473245a31cdea9b789786f4108e4b", "AErxJJxGbc9cZzZoZepN62BNfg5RXns8tmEc3Zpondo"),
    ("CEGon", "0x65d84f0990b7394209d591380c2952c83d778aa3", "7NWHifsBnn9DimUeNnsHdEXkTZhXmJTiXxcCngBondo"),
    ("CIBRon", "0xDcd4536508060dab8F43C334B3a6C72c39528DA5", "BVdL3WUxtxUD4vXRWwqChJLbGxvfzZjBGPp63Wtondo"),
    ("CIFRon", "0xdad07d0ca26ed4109bc00893dbee3ed4ce8ce2a4", "WNZBSkNBNP3Ct1pcFn6Fu4sZQFhnu48EsM9voCEondo"),
    ("CLOAon", "0x4ef383f521e803863a33fca8f3f861e53ef9ef9b", "t71FyTYHVkPAb5g48adDHmkVxXYbUuP2eq6jDZLondo"),
    ("CLOIon", "0xd7e3317d54473dab04135fb0676623f237ff5ca9", "ucQ3VfWAx9pkCN4Kg84zE56FtB4FJN2kQH4ArYYondo"),
    ("CMGon", "0xaed5985afc12aa09d87f55b4b1e6bc3b8f7b0208", "5owVsVFSHACQuippFYdLp3qWRobp2EGcwxMmsr6ondo"),
    ("COFon", "0x53a8c5fc5643b437779742f494691e6b7c660a8b", "R2uDbMtmHq5xSS5SserrovdRKdpiqnVBCd2AHLhondo"),
    ("COHRon", "0x0585756aAFB241b0f8A9Df62Db26c566091Bde0b", "BXMkru8ded26p71gJ3AMMwJmwZaYYfQjRo8vbZzondo"),
    ("COINon", "0xf8589b526fdd65f7f301c605a6e04f0f1b4b3620", "5u6KDiNJXxX4rGMfYT4BApZQC5CuDNrG6MHkwp1ondo"),
    ("COPXon", "0xec93fe7ff4b09ca3ccafbc4cc9615e62be412780", "X7j77hTmjZJbepkXXBcsEapM8qNgdfihkFj6CZ5ondo"),
    ("COPon", "0x0d586b51a90dc999f9bb6a0506da7f034a1d3a2e", "X68p9qTpEMkR1TLpXUP2ZJo8PG4Qge2Y2ZLdjA2ondo"),
    ("COSTon", "0x34375f826fd3dd4e15f883d4f4786bb45eb705ac", "6btaz134wjHkR8sqhAYrtSM6tavftfxnRvnyMd8ondo"),
    ("CPNGon", "0x19904bc04c09e5d29ed216ddd105bdf103a0ba2d", "NKyzy31w2J7odLb2CW3Ft4fpKXkW3LBt1pvpkVLondo"),
    ("CRCLon", "0x992879cd8ce0c312d98648875b5a8d6d042cbf34", "6xHEyem9hmkGtVq6XGCiQUGpPsHBaoYuYdFNZa5ondo"),
    ("CRMon", "0xd04a2bb053277721a8321d7441eed5b42fdf7250", "7D7ukbcnUNYt7Et5vtsDZhAy28MKu9pkHka1Hp9ondo"),
    ("CRWDon", "0xe6837794fbc6dd024733a1a31f86061296fa2752", "cdKfoNjbXgnSuxvoajhtH3uixfZhq1YXhQsS1Rwondo"),
    ("CRWVon", "0x76E39171Cb665a35981e744e2CEB7012F76caEAc", "BfPGpgNyxe6rjAru1EJarjSBAcCABuMF5L32v7nondo"),
    ("CSCOon", "0x34304f2f7cc487eb4186e6d69f5905a613474aa2", "7DWcZE1uVc8m2mf9pV8KNov28ET7HsvHkhrhgr9ondo"),
    ("CVNAon", "0xc145dc2ebdbe8ead1fecdebf46c76eb1fdd0104d", "FGmUDXqA3AbWfo5b3NUcsvwoUFCF4tr9ea6uercondo"),
    ("CVXon", "0xd3113a0ad20a46f6a662c63fe8e637f7713e59c7", "7tgKziACteG26VjV5xKufojKxwTgCFyTwmWUmz5ondo"),
    ("Con", "0x8ddb97556f6ae98b4d408c56b167139fe1cbe3e8", "PjtfUiw6Hwd8PZ94EcUw8mBSYxp7SjjzSLeNTDKondo"),
    ("DASHon", "0x7567c2a46bce46373b454682f3d95e6535bde144", "83P1gCFBZfGRCwJuBt9juxJKEsZwejJoG66eTZ6ondo"),
    ("DBCon", "0xfc2067e3e6a289c205151d96ef67a032f339566d", "td1aY5AvYQuwGD75qNq9aPipMexraN9mQXJwqifondo"),
    ("DEon", "0x90ccbb75d61cb65cd73a3abb5df04a75961612b7", "CqQyAZjB9LGFTG95eiadGTkfhd9QA12ProeKsQmondo"),
    ("DGRWon", "0x1cd89241b26fcdc421fd02907d6504c8abbfe1bc", "gnoSQSNTNZHViqVfxCcPDVxcRA29mrJL7C6JqYLondo"),
    ("DISon", "0xeee9eee593cb8f7946260b4066cba7907f40acfa", "mJf1xT3suXtkXBCfZcE9oUUuyxkvSgqYBWiX7v1ondo"),
    ("DNNon", "0x70bd780076e25d087ed9c35f4e4a540522abe8cf", "12J2LD3tuLfdiVKnWZMHRMrbnXDY9rM4yqVLUa5yondo"),
    ("ECHon", "0x551f8Db0da800C910E12Cf991eac306714481685", "BmXVAFyfpW7VuVYeWDtbFtLx7sek2mZt3BEsGgAondo"),
    ("EEMon", "0x00c81d35eddf44c75d4db9e07bdcdc236eb0ebcf", "916SDKz7y5ZcEZC9CtnQ5Djs1Y8Yv3UAPb6bak8ondo"),
    ("EFAon", "0x38b9a53bfdc5dba58a29bd6992341927c2fca637", "AbvryMGnaba9oADMZk8Vp2Av6MtczsncGyfWaC4ondo"),
    ("ENLVon", "0x5A9D924FC336A5EC8cf3B1909aA660533B50b015", "BncvtBGs4JqgYZwUoq3EN9q9HUFqJKTfWpvCsHCondo"),
    ("ENPHon", "0x30938154E2697694F41592C2e48459287dEBe4BF", "Bp26APthMuM46gMFTo5KYpo7b92GN2xSCor7f9oondo"),
    ("EQIXon", "0xe4e12c9cec3e8cae405202a97f66afa695075fa0", "aheEdmuryJU8ymy8LjYheZH5i2BW1UMsfuWQKD2ondo"),
    ("ETHAon", "0x04B16ff1F9673146F68AA5d5F57aA45AdcF068E1", "LitNUakTges74cjDJm6HHfFNKGPdySkp3MWSYzYondo"),
    ("ETNon", "0x4697b2A050f7B5A8e1ebc27c325f9D78D094f041", "BpYiU1dBXU1fdB64jbR93wHEw3Y47QeRLZvUyLQondo"),
    ("EWJon", "0x82715299f3f132FB85f3De1F7e8fAfd3d79f3eB5", "C6c7VcxuUYcV5YTsky5HM4PUmfwHTwsDD5DNwwPondo"),
    ("EWYon", "0x12B7aDC48416A103F63E7e6210f62C81dfB91fD0", "C8pSaSgjkiTWixS3GM6Hxd6HKnKrgAbY9WDgfVeondo"),
    ("EWZon", "0x9876F4b879cDe9Aa49Ffd260034A0698B7B33A49", "CBKcmEvVg5EgE3W5hVSPcBYWh6TFVjQwbmYod9Pondo"),
    ("EXODon", "0x92d504158A8Dc69de989dB5EDe3230D958fb8630", "CJRoTbu98waCCuLFfLuJ2kXawLk889fqW4UAAbwondo"),
    ("FCXon", "0xE3b17E6D290A0F28bd32aF4064637057627004D5", "CY8ttw5rYCT6fFBJwqXofefqa7Ji9E8zfLmhRLmondo"),
    ("FFOGon", "0xEA130432a9feE9ca1A7eDa84028650d38Bd0E232", "CYAwMGyuNSDu7NpuccNwcxMNS5Bu9akxU2Jooyiondo"),
    ("FGDLon", "0xee0d57462f20434030B8262204c00c0eA0399C41", "CYqLHM92EhmF83iNgfN4A1j2ckjsHigRvXu7xHCondo"),
    ("FIGRon", "0x620477782cea4c4171165396f8014edef83a13da", "ZmHxc6Gt27RJKxD2ay6UL4n9yQ7mKAq4XZQUeVhondo"),
    ("FIGon", "0x93fac02b22b6743423381d163aec418178019b7a", "aLDdFsr3VTUQaHFK6yNvQxztvxQ8nxW4AMuSGC7ondo"),
    ("FLHYon", "0x240Eb4859B4537D250cf784cc758c404dA5Fe4bd", "CZ3FxxSto7tsjkSkqMek1C5p3RCFFmkwKqW57nbondo"),
    ("FLQLon", "0x48187890D16aEE64798E02C5BeD510f4dB5694A9", "CZ9GBn1okotqKNUUqoxk4PF2JVi59bw5GWvVo6Dondo"),
    ("FSOLon", "0x54B92Fd77229269Ff6484942C123cCa72f2D6fEC", "BJhPr9SM7uZTZXHeSLYmUk7CjGQq1esFkVxPF5tondo"),
    ("FTGCon", "0xe96f94e10f1265dcc15f83d251f1f6758d2cd67d", "ivBnfPTyuHDNWmMSnbavckhJK6SHZW8h77nZKsEondo"),
    ("FUTUon", "0x5acf40056ed51c8bbcd1b125ef803581ac89a627", "Ao5rKFRQ54W3DKSAtqfhBRPNHewwWRLNLao2JL9ondo"),
    ("FXIon", "0x9b8E987e6fEc8Cf1380C4dcA7071e2C7853AEEA1", "CeFbGYXDmkyfo1TXXzzZ512mtnCCewNohu6V15vondo"),
    ("Fon", "0xb1aba049c42b6fe811766eba61f51f11c57acc4b", "5hT2o25X9tGXipwhLckaUdgnxrZ6Y8eiUwdhpLeondo"),
    ("GEMIon", "0x817942d5de16092656568e9f67f54ccb462f8989", "NrTdGMA3ujUvWXkwXyZKnhoByb32KTjRh5Vo47yondo"),
    ("GEVon", "0x2Aea1D415D45CCF3EaBE565d45DcaF4ea2035b9c", "CgZSv89BL58ybWfWobANKEU8nV9jYfFw23G2DZEondo"),
    ("GEon", "0x5151a22421ed4277f1e4ca4785a07b035d548a36", "aTBfDuLRqYHBiG82bHA7DzwjSDTFre2dRtGH3S5ondo"),
    ("GLDon", "0xfa9a1e901085e269f6d428f79cd5252d8b919344", "hWfiw4mcxT8rnNFkk6fsCQSxoxgZ9yVhB6tyeVcondo"),
    ("GLTRon", "0x15580092796f69825CFf4738Cac55D05D41eaa42", "CgnZbDNzBfaLyJqUtd4esKLShRp7RznQuwP4uQaondo"),
    ("GLXYon", "0xF98B89825233808CD37706A53D2b4Ae3e359d442", "CkWmEM2J79k6AjAwyQVHXteFucAL1zQrKLxLqJHondo"),
    ("GMEon", "0xdabb9aff4cf02f26d2014e4ca9f94ac6fe6572a3", "aznKt8v32CwYMEcTcB4bGTv8DXWStCpHrcCtyy7ondo"),
    ("GOOGLon", "0x091fc7778e6932d4009b087b191d1ee3bac5729a", "bbahNA5vT9WJeYft8tALrH1LXWffjwqVoUbqYa1ondo"),
    ("GRABon", "0xab2f74804c022c5249d52e743af4340e42f5f3b6", "m9GcsVgdjaL3KsdtSFHimnhtsUMpTHkjtwEG4Tzondo"),
    ("GRNDon", "0x20cce48d767ed68cbba7727c4c504efe5bcb626c", "Gc1aT3ay7FXL3qdAW7cNSXYPDsGavy7qiACuxwxondo"),
    ("GSon", "0x0d4f9b25f81163fb4840ba4f434672543823000c", "BchJRy2snmhJZf3rQ9LJ3ePs2BGfYgfvQNo31d2ondo"),
    ("HDon", "0x31dabf49e4bc1af1456c1819cb6a2562154e92f3", "MtEXKVN3Pcggy8MPA3eJr15H6SK3RXheScqj9qtondo"),
    ("HIMSon", "0x4693f6f5ef257381a28afd0673e64d8b32d5c6ad", "bdh3njeo19d2TBLAKTGvCWdSoArfVw8uZBAJHY4ondo"),
    ("HOODon", "0x19601179a60f55ff6636f5d1a8b6671053bd60a8", "BVdXGvmgi6A9oAiwWvBvP76fyTqcCNRJMM7zMN6ondo"),
    ("HYGon", "0x0dae81a905b645a3d1e67129b89cd0acda224e9a", "c5ug15fwZRfQhhVa6LHscFY33ebVDHcVCezYpj7ondo"),
    ("HYSon", "0x75E9D68e99e76714ed1a7663ab48ba3AaBd7A6c5", "CsN1Tyz467bSFLPGd6MJyZhPNtwDaWZtX8ixHWyondo"),
    ("IAUon", "0xcb2a0f46f67dc4c58a316f1c008edef5c2311795", "M77ZvkZ8zW5udRbuJCbuwSwavRa7bGAZYMTwru8ondo"),
    ("IBITon", "0x68B07cEf227Cea1b2b6683921C8c825cd5C69Ec7", "6JLG8iUkAuqiBhL3j2ckDMDf5oWAa6awmyaWezKondo"),
    ("IBMon", "0xe8ff70859ce4cbd72e4352b4fb45f5bf39d07464", "C8bZkgSxXkyT1RgxByp2teJ24hgimPLoyEYoNa9ondo"),
    ("IEFAon", "0x918008c3d29496c37b478b611967beaca365af36", "C9J9vZ8N79GzzxFoRkPWCkGtMKU8akg4FhUk4r9ondo"),
    ("IEFon", "0xA486a0A05250E8621bA3B26C3bbc517145eba619", "D4uWxzR5StYC6sTRhVts8Eboy3pmVtHeNC62dnQondo"),
    ("IEMGon", "0x22092c94a91d019ad15536725598b0a6be0a73c0", "cdVNL7wK8mf1UCDqM6zdrziRv4hmvqWhXeTcck2ondo"),
    ("IJHon", "0x167e93a849a0cc479769132552b99aa1cfa0948c", "cfPLN9WXD2BTkbZhRZMVXPmVSiRo44hJWRtnaC8ondo"),
    ("INCEon", "0x5e24DB6DE4C21E2C8f9E81bAcfCedFBAC2DeE4aa", "D8KT4Jd8qiKKTfkM8ejSKCpWGR1o3GFvnQGp5ERondo"),
    ("INDAon", "0xdB0748297fbEf0B33dF89e86519A0BD3adAf6459", "DBNwt3FoYCKQWdfzxKFNZ4mzuz4Jz1iRzFf7HFzondo"),
    ("INTCon", "0xa528caaa2f96090e379d43f90834c75df54d6e74", "cJpUMp5R7rZ6fGeLHbHhrRuJzK9mkyKDjZqNpT3ondo"),
    ("INTUon", "0x6e3e077a6c0e3c27fd6d00b97387d9b7bd451bab", "CozoH5HBTyyeYSQxHcWpGzd4Sq5XBaKzBzvTtN3ondo"),
    ("IONQon", "0x40d8E1fbaf69173c47fa493FeB50a84eeC6b57eE", "DDZQijTbaSd3Kas1r1bgCnHPayk8vTP8SfZWp5Tondo"),
    ("IRENon", "0x8fd70ee385f470c8d6fda2d93a4e49c849bac6a6", "13QHuepdhtJ3urNsV9i1hdL8nQoca2G7ZaLzb5FYondo"),
    ("ISRGon", "0x784584933c2192caa062e90d8140d94768ce62d8", "1MGRpPrkhEsCm2GCWD3rsvEU77xTTLAzfKXeFgFondo"),
    ("ITAon", "0x88B90f45bd6a4F97f7D85d280eD64A40880e4935", "DDcAL93Urf7KrPntvKULnZoFs4Wdee1LkkJqLpjondo"),
    ("ITOTon", "0xcf9caf83053213c44dd7027db3e1e4ac98e55f8f", "CPWkMURVvcnX8hGjqCTb8i5LkzV3VSvyk7SeJi8ondo"),
    ("IVVon", "0x1104eb7e85e25eb45f88e638b0c27a06c1a91cb2", "CqW2pd6dCPG9xKZfAsTovzDsMmAGKJSDBNcwM96ondo"),
    ("IWFon", "0x40755f06ab7f8de1ab3a9413b1ef562d63de19b1", "dSHPFuMMjZqt7xDYGWrexXTSkdEZAiZngqymQF2ondo"),
    ("IWMon", "0x500eafc69b68acd6f27064f9b75f1c7d91cc4d9f", "dvj2kKFSyjpnyYSYppgFdAEVfgjMEoQGi9VaV23ondo"),
    ("IWNon", "0xf54b94ea21e1da5d51ef00fd4502225e5394f874", "DX7g7WNjDpVzNK9CG81v7wb6ZbiNzYfkdzH2Xs5ondo"),
    ("JAAAon", "0x84719a1082ed487c7eeac7d69885e3cc2009ea78", "KZtqx9BJbpcGY7vdzhqPXM3ECKChxE5YhXaDiwRondo"),
    ("JDon", "0xe92be960ae64f6a914ca77014cac9e56de7f36c1", "E1aUS5nyv7kaBzdQzPVJW5zfaMgoUJpKYzdnFS2ondo"),
    ("JNJon", "0xd1f799cb9f5d0a02951b0755beced6c43882712f", "KUXt7LzHWSQXp5eyqMZRxWjAP6yM8BUh4LRHwiwondo"),
    ("JPMon", "0x317bf42b43a394860718266dec445dcc9fd9da49", "E5Gczsavxcomqf6Cw1sGCKLabL1xYD2FzKxVoB4ondo"),
    ("KLACon", "0xfc263946439b0d802bf4c5a6fcd34e2885259f91", "149o8ppQf9SzKCKXZ4v3dzHkwumvtQSRzSEkr29uondo"),
    ("KOon", "0x405f38b90bebf1259062cf29da299f3398662bcb", "e6G4pfFcrdKxJuZ4YXixRFfMbpMvgXG2Mjcus71ondo"),
    ("KWEBon", "0x7437203800140BA7d9081ddE8cEF09EE40E3Bf03", "DVPSYdqWPLvNa8afnEqa3B9eDfTTWpGyUZeXvdMondo"),
    ("LINon", "0xe1743616f705954620aa351465c8885fbde5a8a9", "Edik9MoFp8LAXS9HNu2gRFyihwYqDqv4ZmNmVT9ondo"),
    ("LIon", "0x9810beac9af3c30d14cfb61cdd557e160f60fd50", "v12TwfofSbvVqQ5N5KGG4d3J8rtEi4BjGfn2apyondo"),
    ("LLYon", "0x341d31b2be1fee9c00e395a62ba41837f4322eed", "eGGxZwNSfuNKRqQLKaz2hc4QkA2mau7skyxPdj7ondo"),
    ("LMTon", "0xd09f7b75b9659b864c6f82bb00ff096f9d277998", "EoReHwUnGGekbXFHLj5rbCVKiwWqu32GrETMfw4ondo"),
    ("LOWon", "0x2ec46eed30c94caa5979e6a0395abe824138335f", "edLdFJVVR532qhcrNTJjLAmhmyV7NsctbWVokMBondo"),
    ("LRCXon", "0x35895a1fa1aff7fb3204fb01257409fd75acb24c", "wFJoeEYpKg9oRhyJy6BWTT3J95gmXBLvoeikDQNondo"),
    ("LUNRon", "0xA3b7B7cfEb023a6C4f444f5ca9a3Fc85809Ece15", "DiDWPZ7vQXfpaeQ8BX68XuDYeiQLv7diDxdeUpaondo"),
    ("MARAon", "0xd226d8170ee38793430c7dec6903df4b818bb74c", "ETCJUmuhs5aY62xgEVWCZ5JR8KPdeXUaJz3LuC5ondo"),
    ("MAon", "0x25ffda07f585c39848db6573e533d7585679c52d", "EsVHcyRxXFJCLMiuYLWhoDygrNe1BJGpYeZ17X7ondo"),
    ("MCDon", "0x995add4ba29a628a57930a8a185c62ca044ec090", "EUbJjmDt8JA222M91bVLZs211siZ2jzbFArH9N3ondo"),
    ("MELIon", "0x60a8f8e05200ff73afde9e2cae819bf1605f0bdd", "EWwdgGshGngcMpDV34pWZRSu5bkAuiKuKTTHKQ8ondo"),
    ("METAon", "0xd7df5863a3e742f0c767768cdfcb63f09e0422f6", "fDxs5y12E7x7jBwCKBXGqt71uJmCWsAQ3Srkte6ondo"),
    ("MPon", "0x4baf4dc56cf6a525a0874e25cc6372a6a8915135", "XwFm5GiKPVTvPiEbQpdc6vJbFEpsUXRMf6TcSxnondo"),
    ("MRKon", "0x869027261075c3c239d6a26842579b93802606f4", "bn1fb8dwzafGePqNPrM8m8cbAKQiFqeEPuZkPySondo"),
    ("MRNAon", "0x01486675da0764ee780ea7cb65c33062e9b2d28c", "14VP7DvCAdBCc5XGNZkPt6zhtPzJrWWS64Koxtxyondo"),
    ("MRVLon", "0x1501ec83ffef405b4331cc4f73277a40fb0c627d", "FovBwhoV5KQjZCdhoM6jgXYwXLX3F8vgAfvmLH7ondo"),
    ("MSFTon", "0x6bfe75d1ad432050ea973c3a3dcd88f02e2444c3", "FRmH6iRkMr33DLG6zVLR7EM4LojBFAuq6NtFzG6ondo"),
    ("MSTRon", "0x7313ea16493b2f55054df0131a3a14b043ec8992", "FSz4ouiqXpHuGPcpacZfTzbMjScoj5FfzHkiyu2ondo"),
    ("MTZon", "0xf49046aae76eaeb7ffd3ef116ce0f7cd0f52d93e", "R3ywbVQ5t8LNmjQsn2Ngv43dSqyZscQwNag9G3Eondo"),
    ("MUon", "0x8b6acf6041a81567f012ff6a4c6d96d5818d74bf", "Fz9edBpaURPPzpKVRR1A8PENYDEgHqwx5D5th28ondo"),
    ("NBISon", "0xee268780473E7a0e47baC41547C6E01512555A16", "DiRshqNDE68bWbGdLHm1GwQ76MvWQG3af6w1NdQondo"),
    ("NEEon", "0xe9d43f7e6b2237e8873a7003b3f43c6b03160be5", "t7eN6cGwRMFaZvsNW2SmVwkedmHtDdrxA4ycNE5ondo"),
    ("NEMon", "0x5E63232993789601CE362e0240a299C1DfCBfbEc", "Dig28Tf1ufhCBAsjTmFkXCgcNgMqDMYj5A2rDQmondo"),
    ("NFLXon", "0x7048f5227b032326cc8dbc53cf3fddd947a2c757", "g4KnPrxPLeeKkwvDmZFMtYQPM64eHeShbD55vK6ondo"),
    ("NIKLon", "0xe23f03d2907cdc38a10f6ccdc1a157bf1afe51de", "V8LRV7kWjrx6Prke9oHEHNUiR122BVtyuPciTCTondo"),
    ("NIOon", "0xc6f9edbee6042a237d72493bbda3ee2c3c62f708", "yQ37dFiGAbzrb2FRAEhGNzRy5zFfoYGWYhAepFEondo"),
    ("NKEon", "0x04b5e199f2ec84f78b111035f57b16bee448db6f", "g646pcdG2Rt5DH9WZzL7VVnVDWCCMTTrnktwE74ondo"),
    ("NOCon", "0x4D3442D884202584F1729bCA20Db05472B886B52", "Dm6FpQ76SsbVmAZ4NvD2mjZP7cxbw1CASr4WwCiondo"),
    ("NOWon", "0xeb19c13c54b1cd48afc62f6503375e92d5f1e856", "G7pTVoSECz5RQWubEnTP7AC83KHUsSyoiqYR1R2ondo"),
    ("NTESon", "0x282973969118f9fe39bf2ff3d8dd1efee82ccb11", "YeK2TdPtGLAme3Phg4pb1GBN2YxKgX5UNVyD4asondo"),
    ("NVDAon", "0xa9ee28c80f960b889dfbd1902055218cba016f75", "gEGtLTPNQ7jcg25zTetkbmF7teoDLcrfTnQfmn2ondo"),
    ("NVOon", "0x08a513779f46ffb7a34f16094a94016d010128a8", "GeV7S8vjP8qdYZpdGv2Xi6e7MUMCk8NAAp2z7g5ondo"),
    ("OIHon", "0x31D6011023D6c7695Efc29bB016830F3F36De40a", "DnvbCqRuUYssmKVRBRNwkUnptHitH4ZZTt1KVuZondo"),
    ("OKLOon", "0xaf6c03acf72355ce98d0741302b78870b376428c", "m6oDLvJT7rY7M1TxuLWP3pWmAPg2cCWDQR1NKiEondo"),
    ("ONDSon", "0xd85d4ce29b4ca361ff72ef0e53d6236e334c5db6", "7qy1j4Mechfyr6uAST3djH4vk4kiEYC2cjEytXdondo"),
    ("ONon", "0xb35a9eab5d25282f4e668798b629a9294e9a47aa", "13qtwy5fZi9Przz14pzo9xqFSr8QHmLyUpUCvP1xondo"),
    ("OPENon", "0xa09699fc0cbb1f85128450a0ff6a3c4d3a7e7b9b", "ou1uE526v7zmUYP2qCb2LJgfXAyWAtWS9SETtr8ondo"),
    ("OPRAon", "0x88672043905bdd272df55a5a7bb1b7e1e693cbc5", "gbHFTMkuMQUy5xrgoCBdaQ2XYvNyjWAYcnRPh9Condo"),
    ("ORCLon", "0x03e4bd1ea53f1da84513da0319d1f03dd1bbcf93", "GmDADFpfwjfzZq9MfCafMDTS69MgVjtzD7Fd9a4ondo"),
    ("OSCRon", "0xb0752aa50b089ee6ea9acd51373207fa460e87bb", "ThwGDsXZ6iKubWuEQjmDxGwF3bUERDGbBXvcbjFondo"),
    ("OXYon", "0x01b5a4ac600be98448dbefbb78bcdf38262552cc", "1GNFMryQ6c9ZpMhgNimmsbtgYM21qnBJgRAFoNiondo"),
    ("PALLon", "0x3fcd741646a9790635b938cdb69af5df356cbaab", "P7hTXnKk2d2DyqWnefp5BSroE1qjjKpKxg9SxQqondo"),
    ("PANWon", "0x0eaa1a75bd682a5669ab2371a559fbd039c6b9eb", "M7hVQomhw4Q2D2op3HvBrZjHu9SryjNvD5haEZ1ondo"),
    ("PAVEon", "0x6f28Cb07790c1049ecd7482d09Fd13B977B47201", "DsLQ18ooPjiHYuiuQ5Jz8PNCpVaKe3FhAYpvMxWondo"),
    ("PBRon", "0x2b1d5cdecc356530a746c5754231efaeaca64022", "GRciFCqJ5y2hbiD6U5mGkohY65BZTXGuGUrCqf7ondo"),
    ("PCGon", "0x47b36ddb9dd12a8411f78226f55e8c3f0d65481f", "UP5s1srLaHDc4SwJqLPa3A48x5R7ofN3hZWxWEZondo"),
    ("PDBCon", "0xcf3e84e62002ca459db81b2032d7fe13715bad51", "M6agiXbNgy8Xon9ngiW4ZDPbMFcNCTMkMMkshZyondo"),
    ("PDDon", "0xf3e82ea164cb344b2b11bad4c24b0ea4f7ba4714", "PnjETBCLC318DRejo9cMQKAmET9PvW8AEFGWMNtondo"),
    ("PEPon", "0xf99f8f3a95257d82006183bd524efa7aacc9ef7a", "gud6b3fYekjhMG5F818BALwbg2vt4JKoow59Md9ondo"),
    ("PFEon", "0x8a83c31d6751833b4940b6e871c48d9a15a07b46", "Gwh9fPsX1qWATXy63vNaJnAFfwebWQtZaVmPko6ondo"),
    ("PGon", "0x400f1e257f86d25578a0928c94dc95115f09d5c9", "GZ8v4NdSG7CTRZqHMgNsTPRULeVi8CpdWd9wZY8ondo"),
    ("PINSon", "0xcfd1f0df84300ea1a4e2ba5238043a2fa5a7237c", "sxyg1VTSzy5zYANUK7hntNtmFAWoXGJq95AcHuVondo"),
    ("PLTRon", "0x9351abd19f42101dd36025e495b98e910b255d78", "HfsnTS5qtdStwec9DfBrunRqnAMYMMz1kjv9Hu9ondo"),
    ("PLUGon", "0x4752ae8f910b25e64e4406eaad50c1b4e8de7e6d", "TnfswqdE1jAJ8sfnf5J7kSVLEH1cfpAYZ8MWmKfondo"),
    ("PPLTon", "0x3EC23F52F6573FC0587A0631dd8C3b107f6bcb35", "DwRtkbsaQMGAS3oMeEGYh6M5vH4X9WECsQgqHjAondo"),
    ("PSQon", "0x3802dc739ef9e226f36421a9c15efa519153bbbe", "qKtU9A7ij34XmtxaSzYfxCpkgAZzzFsqnUb2kW2ondo"),
    ("PYPLon", "0x374d03a6c0d5bd4be0a5117ebe1b49d52ac8a53f", "hM7B3UQTTR81mS27SxDDPzBbjejmo8fnpFjzgv9ondo"),
    ("QBTSon", "0x8c7bf0ed6bc778bde1489de1592c1aad3e66371d", "hqJXutLF6f7DxStrWCrnZDfXzbNTZmvi3KheVi6ondo"),
    ("QCOMon", "0xfbd4d681c92ead6af0e49950c8b2e47eeacbb2db", "hrmX7MV5hifoaBVjnrdpz698yABxrbBNAcWtWo9ondo"),
    ("QQQon", "0x0cde6936d305d5b34667fc46425e852efd73559a", "HrYNm6jTQ71LoFphjVKBTdAE4uja7WsmLG8VxB8ondo"),
    ("QUBTon", "0x82E07C1017032cFd889b1Ca81EBe722c4D4de825", "E4YowrHx5wm4RtSjfuvTqtNH3Wf7NEj5tYZGD9Bondo"),
    ("RDDTon", "0x4da12f47578ef89c76179b760c778e70b668f80b", "HXFrTf9v9NdjGUTnx4sojR3Cf92hoBsQFUxKTN7ondo"),
    ("RDWon", "0x23e39D94807a8bb7e3f8294b4911D04EE26DcE39", "E6KSaqjvqe2HiUpbEweRxLK4RimQddigm95H9Jaondo"),
    ("REGNon", "0x30BD85fD4286c5c9857679F5B188f737B4a7B8C0", "E86mX2yb3HLbJM6gRtZQ6dCYmLh6MSDZadu9SCPondo"),
    ("REMXon", "0xc16f47c4a7ed39372b9a0e3e2016cede9b4cb83a", "tiitb2Z1HtpB2DpVr6V7tdCFS3jmTinLeuGj9EVondo"),
    ("RGTIon", "0xed2a500eb2b66679e0bbd76e51a60049ae5f3271", "dwEPNKQab3iwRmjGvZPXhAmws1W5NsQGwuXwi8oondo"),
    ("RIOTon", "0xc4a88a72b848255fd24da3c1ad6755d980535fb1", "i6f3DvZBuLpnGSqS8x6WPeStJ7jNe5KewD6afD5ondo"),
    ("RIVNon", "0x277e1fa8704c5511fed7e30bc691f922aa30101b", "AXRsYFt7TXNQ3DcY6BkvRgPV6VsYMURyDtaeudjondo"),
    ("RKLBon", "0xb4D695569236273745B4CD54B539b1b9Cc1513af", "E9VQY3VnrpVSekFByzRmfeK1kxgM3UiKCoVVbdUondo"),
    ("RTXon", "0x44fde2c6bc2c2b54962c69fcef57a2a50121dbd7", "12BvLZtzjdssAycxPeBQUjukhmgQpULAvy6SroYdondo"),
    ("SBETon", "0x99e01f02d66455bb106d91d469c9eaf6ab4904f6", "iLDu2jjp2i3Uqc2Vm7K7GLiUj3hR4Un49MtD7c4ondo"),
    ("SBUXon", "0x94d7754541b829a87321d56121bc544167ac490d", "iPFqjcZQTNMNXA4kbShbMhfAVD8yr8Uq9UtXMV6ondo"),
    ("SCCOon", "0xF15B8f7465b92799F6EE440F86B3CAB5A4dbc65A", "EANjzFjj3nPXHdzN5CE3Z8LLVn69Ce77FE8X4cvondo"),
    ("SCHWon", "0xe5ba472c98b7e4695bd856290de66bdedaffc123", "cnc6M1zXLdrGR5LAQVcaJDfgezMiVWNtGQsVy1Kondo"),
    ("SEDGon", "0x8755c5C39b1AA9053a83AC731242a2cf4D04B0Fe", "EAwP9LGNjTkQ2YeKE6CGKqBYtrJ6APFvRe7KCMmondo"),
    ("SGOVon", "0xc008c5f579ec1450f20099c39f587547e27c7523", "HjrN6ChZK2QRL6hMXayjGPLFvxhgjwKEy135VRjondo"),
    ("SHOPon", "0x43d0b380c33cd004a6a69abd61843881a2de4113", "ivdDracs2s7jCP698dJXKSEQdVrNj9hasJL1Uq1ondo"),
    ("SHYon", "0xf95e50BE5Efc96117c28775F80C7Cdb41Ebc4888", "EEy57xbaLcUrN1HXj2vz8VWxeWFK1eZQZo4aWbrondo"),
    ("SLVon", "0x8b872732b07be325a8803cdb480d9d20b6f8d11b", "iy11ytbSGcUnrjE6Lfv78TFqxKyUESfku1FugS9ondo"),
    ("SMCIon", "0xc142ba8ccd36d80c3a001342fb83e4c3d218a873", "jLca79XzcewRuBZyaJxVxuKpUHcEix1X4CP1RP9ondo"),
    ("SNAPon", "0xf325884d9bcac457271fe7f7b6be1765348fcca2", "a2cXfonVgQ6cKB4Lm8YZsPry39VZSA562bwmRSiondo"),
    ("SNDKon", "0x4Fd67CB8CFEdc718BAc984b5936abE3330d0a2A4", "EJmUVvDqAdfH5zEohkdS4234bi3c6iunqEMobjmondo"),
    ("SNOWon", "0x138ed6833ff4e8811e1fea0d005e13726c8886f9", "JmFLCBwoNvcXy6B2VqABg6m784ubkXpaEx3p7S5ondo"),
    ("SOFIon", "0x71507068e98049cba81e9bbc8d901e4a2f4222eb", "mqL8yXQpeSvc7NgrAtLLPtRvUiWyLoG5RWLv16iondo"),
    ("SOUNon", "0xedcf71b2e2217064038adcb54a3c3a5fc3488ef1", "vE2qArmjto6VfeMngyGAnzp2ipLYeXsxiARDnnXondo"),
    ("SOXXon", "0x2A3cbF64C8181DB4a25D41D4d7a7Db9984C59DAC", "EN5pHc1LccUSojxb7kkyQi7v7iJN5RpDq6qz3DHondo"),
    ("SOon", "0xd7a6353a23ed2c4fcac29a63cbbe3f65ffef41f5", "aKzjn2ZdWySSGPSSDTY2HUpcSCmemSahTXihrpyondo"),
    ("SPGIon", "0x55b370b704240a914f42b5bbb3195431c031f9f8", "JrTYw7A9jihX5TwpRStYviEbsYf2X2VJpZ13719ondo"),
    ("SPOTon", "0x50356167a4dbc38bea6779c045e24e25facedfdc", "jzCvs2Pk8tDcfsFRqnEMjurgaQW4iQfEkandUR8ondo"),
    ("SPYon", "0x6a708ead771238919d85930b5a0f10454e1c331a", "k18WJUULWheRkSpSquYGdNNmtuE2Vbw1hpuUi92ondo"),
    ("SQQQon", "0x17515b68378d86c38f394c666e79907da05dcba9", "D1tu7Fnm3cCpKyyPXrqm5GXShPqMj7a2SEjjq9fondo"),
    ("STXon", "0x966EbCBA3c51E81f5CF159a1EaBeFd2327aB5E8D", "EXtprP1wzrNo2bByrU9JyzqEg2hQMSCVJakeHHYondo"),
    ("TCOMon", "0x6459303f58244ff1e7a42b90aa3782dfb6ca6969", "9PMjLqd8zPdKkJUXarnit5t7tPL3cCscwHzy7ATondo"),
    ("TIPon", "0x2ac26ec236df5d1d2ad1a6dd4e448a90e45dc35d", "k6BPp2Xmf2TYgrZiUyWfUoZBKeqaDbvPoAVgSx2ondo"),
    ("TLNon", "0xbbe4dfe7a349fb72aec6f52d5cd9bdd78ae8f313", "RTb54gpqAx6RpLAHRGnqQ3ciQ845CHqhg21ZzEJondo"),
    ("TLTon", "0xf69e40069ac227c11459e3f4e8a446b3401616b6", "KaSLSWByKy6b9FrCYXPEJoHmLpuFZtTCJk1F1Z9ondo"),
    ("TMOon", "0xbcf7d958791152128710565a5fc6f68342ed71c8", "T699bgtXQw4CJ59rQ4VzLsupVQUzoL5RmuhHnKrondo"),
    ("TMUSon", "0x2588f20bad92da8dcce7fac8311b5f8ab4690e43", "pDY4GPJfZcNETPG7myXeafQfgJqqVkn81bMYDyfondo"),
    ("TMon", "0xecc1299f183b6a720a6f4729bf24f82cd8d50828", "kbmF7ERJWMaaDswMprrH9gHSLya5D2RMBNgKqg3ondo"),
    ("TQQQon", "0xe42cfb20e00912409b77a602b5bdcff3c7acc5f4", "14W1itEkV7k1W819mLSknFTaMmkCtPokbF2tRkPUondo"),
    ("TSLAon", "0x2494b603319d4d9f9715c9f4496d9e0364b59d93", "KeGv7bsfR4MheC1CkmnAVceoApjrkvBhHYjWb67ondo"),
    ("TSMon", "0xc37042a7a4fa510d8884a433762ab87257b91965", "keybg184d4vyXeQdFqs4o99YsMg7xBthxTJ6Ky3ondo"),
    ("TXNon", "0xca3a5c955f1f01f20aacf9501b03e4aa235e478b", "81xLFvCzFaUM3KDxSHC75pXu3RPCeSeCbmGBY8aondo"),
    ("Ton", "0x4255279af47cf10efb9a5c8839f90170f4ef759f", "WKMZummev5UcXz5nNKQZvTD6QjNSM2X58uwmDReondo"),
    ("UBERon", "0xde9d6036fca870f7efc5a82722ae694c371ac909", "KJNeFW3kk3ycPjXpC6cbuyckjeYHacc2ekhtAi5ondo"),
    ("UECon", "0xE7ddF606841ee278A30E5C90486681e68ddd8cbF", "EYo8D3cLdF1CDeGms5M5VHyU52HJYinkMZ1cqvYondo"),
    ("UNGon", "0xA5351C9bf08055E03642b6b8649A0f7e895501BF", "Es2ipHL7qXBcLmZ4N7LP9PHBHaWaTMTAkxDwGGjondo"),
    ("UNHon", "0x3385cb29cca0ac66f5d2354d13ef977b49a2510f", "kPBGL8vAwKN3UGmr9cjkM2dU79SC3nzTC9yu7F8ondo"),
    ("UNPon", "0xfe9aA194E3C4604f3872f220eb41C33A287FCD90", "EvsME8gdnEwPLbTnhrGVDwrY35zBuB8hEGCq59Hondo"),
    ("URAon", "0xc7806943663158D68740a14ab0B270bD60BDe87D", "EvzskrQ3vUUkiMGG1DzfSDyG6H2WCMy3v9G8fzzondo"),
    ("USDon", "0x1f8955E640Cbd9abc3C3Bb408c9E2E1f5F20DfE6", "ZPFtoCe7WWqG4N3ZFRccS8T9SMBeHsd1Vmgv2i7ondo"),
    ("USFRon", "0xf4fd75764a5c086fb12f822be2ca318b3a362dc3", "o6U1Sm6Vd7EofMyCrL28mrp2QLzgYGgjveHiEQ5ondo"),
    ("USOon", "0x94174e3d1335db402dd03a092f7aa7ac2cb32be4", "rpydAzWdCy85HEmoQkH5PVxYtDYQWjmLxgHHadxondo"),
    ("VFSon", "0x1D2EaAF0aE00382893aa4318Bd88d1Cd0e9B858A", "F3V1fKLKv7H8aNdt9TC6GQ3X4LayEfGHsPi8Umaondo"),
    ("VNQon", "0x10b58A3d9DCeC59bB1c3bf6b9c9414eAfCE711C9", "F3dMJ9H137YUNc9cpN3gBWDSq4MSRbTFtojH65Uondo"),
    ("VRTXon", "0x8c9979Dc208f74a5602c38691aa920F121e2f863", "FL7QzUq58pvkDxkftJm7RqRWgqYEFZwXuvAMsUnondo"),
    ("VRTon", "0x9cea8a7be1ab0320b709d368ad60d8500f55995f", "MkN2TZSYTFBdMRLf9EVcfhstTwnazH8knd9hpepondo"),
    ("VSTon", "0xf2c24c47805f4f72d3919c8674bfdd401505794b", "h6MW8GFpfzxFa1JNn6hZNnBF3t4fj9SHAXKy6LXondo"),
    ("VTIon", "0x158734153f354cb326ee690c3d55f810dcb0fc90", "jCCU4GwukjNxAXJowG2S4KCrr5g6YyUB61WHYvGondo"),
    ("VTVon", "0xc2dd31b1b3a2f515ce0d48de712c6744c3475170", "KuiYLPVq65qixD9TgvxBC576C4gG6vVTCdbh2zFondo"),
    ("VZon", "0xa3b089c886e6d721f49def8e050f3b9d4362560b", "igu1coP6n3GPaWmbd8J9Z7UAyLpV254uQFFNfydondo"),
    ("Von", "0x1cde419fae0ef7f7931ae3e29e5f411c8c5e5fa1", "kxEW4oJL75K37VeXaZF1ynbHQATQwhECQKN1374ondo"),
    ("WDCon", "0xcEb29848d04Ad3Cb46E1fE8E45B82ffAc39D797d", "FLqH2jB2DZPJP5nnVFAakRKaNTcDZtq71Pnpp6Aondo"),
    ("WFCon", "0x629520dee1620def11596f84e85de9f1ff653012", "L6ZE5qCpVVSqLePz64CrwkgyWoPF9M7tB8BeFH4ondo"),
    ("WMTon", "0xa7d1e886acf66ec0656df2decb4b7c893a3bab4c", "LZddqAqKqJW9oMZSjTxCUmbmzBRQtv9gMkD9hZ3ondo"),
    ("WMon", "0xCE0466Bae0e867239719dC386CA84b1F3eFE6914", "FPvKvWzSzDZqgYmSZUetrkpUXSwo2VtpR4BynVYondo"),
    ("WULFon", "0xad56701d9e57957e28e546db7db508a16d4f86cc", "exYfSJt6Fgfhfnp3bAD4roYy97hLF9npjYaLyEXondo"),
    ("XOMon", "0x4d209d275e3492ac08497a7a42915899c4dd5e86", "qCYD74QnXzd9pzv6pGHQKJVwoibL6sNcPQDnpDiondo"),
    ("XYZon", "0xe778a2e5d953c82eb9475cf3b87654226a867344", "BWxe2FVciUbwrCUZQPUKiREBh5LmVa5AiUqNLAkondo"),
];
