//! The fake local provider uses a dedicated test-only RSA key.

use ring::{rand::SystemRandom, signature};
use serde_json::{Value, json};

use super::http::b64url;

const PRIVATE_KEY: &str = "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQCIyCmXoMOHTOKuxo6--Z1ukxzrYmpDJ78US12Ngn3qU45wwxGoajKI5pbGkq4v8RbS3cXol33LAsi54S14MBwkhkTnRBkRUYo6WU4256XCw0f2hADLyi9BgIahxBALpL8ZN-Z6C8vP-C67TYm1wSmtczt5H5SaFBC4lziGDQY6N7HLtjMSyjwJFr4N_akmxMS6yXGhoUNDWrjTBQxpqQRhwkUNCwMqBnrgdoRD4EODH-ff5QdSsBOV9-CP3BmxT4LGJnufjCf7lmOCK8JvgZzWot7XRNEhQJ1x7ulXPQuFg8w4iWyXe5suGHLEwZCxjBqS2rkYe2s6SyQr6yM8R0ObAgMBAAECggEAFHdlfc-AzPMXV2Z_am-de7Od1_yjAMcRjh7-xxNM9Zh-D5JTRztosgJVci31dq1Lfc7QSPljSafyJ-9bvIhFAGlqJZ6TZPt_RDQjrLnO8YxYbWLwCY4zBvPicTxjLZLU7e2RhMEqGcVoHKpBSPBiZOvWl3eUZn-fxuGzA_UunDGMNEOe86dxq6WjhnpEc9b-qiTmGNu-VTg9-IWUMQJzYJsOugeTMw3Bwfw9dqETpg7XhrTFF6hUEd7ebyffzw1mH_ohSJ5h6EUdMGZLRrzv-iJOwT9skTvc9bkMRGiTtUpjuvH1wak0lfGO_qWHPmCY0y7mhctPKYLDYhKCA70HrQKBgQC7fV2l94aX8BxEqoYuY4wNKGSkKfaOI_8KWGZY9gGjGDmvGNJ2udQ1-pxBhTfbTrhMY2ZR9M0IlD--hbuZZIIkIINo7AatbWet3oN3rTrcJ41GJMQwjdtBOp1CCnZ9Xc0KCU582G8Xb3I7fZbS8jabK-oIO1xSTy6q_8rmyrux3QKBgQC6w1uPkPu8jnk6Ow4_fG6eAHEpb8UCOSYf2tqoFshEktCZaAqEh3U9h5aQlm6IDO_fzxG78Eu-dE33AerRxSZgv1hDh8obPz3cXOSix2M4U3Hd48yLRgLECZkxZuK88MnjrJPiFvUidO0seBbLcmLjnaXvbRVKk6Y4XTZigiW_1wKBgQC5_xGDTz6HxlzUxfJdXMYYW4UT0slUk2M06SOHh21yVdsL2G0Z-RoL_rU6zqTt_Hca2NUt4tPwedmO8W59KtdsbZDP6q5pKArd0svLUt_8fYRKHLlAjcoAONy_IlAP6Jk7JqZldfUnY5kIaXvRgxYm8_a0-eaJhSH_48hvEpoHRQKBgHGH1j30tYLO3RSv155yJtA3lpLI1erfELTmlmr9xmohEVtf6pzbt0JbDQUB3Vko_ByCRNaJYZH6I3bXHIa-IPFI__-1KP_dNt6hPX9SU5dm6l4nXyz-tymyaASm9YhDm4aiXxdeO0_h4ldY_QpZk5GoQLnGBt5Z2P6Ec034rbfrAoGAA2fSqQuLFZG37aSDZgzrDuiYLr0apXC1y0Jm7D07wxxpy-4O2bMcSMZKv1vSzsh0V4ZvkeMwEt_i8KlWtHBu3OeAmmZOU1m23-TUq0IxGHUSbj1aXphbyGHLUkiTVa6CqVkTBA9MR5vmBypk6w9TH-8mpBa4PQoGEFwpClHTv0s";
const MODULUS: &str = "iMgpl6DDh0zirsaOvvmdbpMc62JqQye_FEtdjYJ96lOOcMMRqGoyiOaWxpKuL_EW0t3F6Jd9ywLIueEteDAcJIZE50QZEVGKOllONuelwsNH9oQAy8ovQYCGocQQC6S_GTfmegvLz_guu02JtcEprXM7eR-UmhQQuJc4hg0GOjexy7YzEso8CRa-Df2pJsTEuslxoaFDQ1q40wUMaakEYcJFDQsDKgZ64HaEQ-BDgx_n3-UHUrATlffgj9wZsU-CxiZ7n4wn-5ZjgivCb4Gc1qLe10TRIUCdce7pVz0LhYPMOIlsl3ubLhhyxMGQsYwaktq5GHtrOkskK-sjPEdDmw";

pub(super) fn jwks() -> Value {
    json!({"keys":[{"kid":"test-key","kty":"RSA","use":"sig","alg":"RS256","n":MODULUS,"e":"AQAB"}]})
}

pub(super) fn signed(claims: &Value) -> String {
    let der = super::http::from_b64url(PRIVATE_KEY).unwrap();
    let key = signature::RsaKeyPair::from_pkcs8(&der).unwrap();
    let header = b64url(br#"{"alg":"RS256","kid":"test-key"}"#);
    let payload = b64url(claims.to_string().as_bytes());
    let data = format!("{header}.{payload}");
    let mut sig = vec![0; key.public().modulus_len()];
    key.sign(&signature::RSA_PKCS1_SHA256, &SystemRandom::new(), data.as_bytes(), &mut sig).unwrap();
    format!("{data}.{}", b64url(&sig))
}
