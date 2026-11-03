//! Verification of RS256 JSON Web Tokens, as issued by Cloudflare Access.
//!
//! Deliberately narrow: only `RS256` is accepted, the signature is checked with
//! `ring` before any claim is read, and `exp`, `iss` and `aud` are required.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::signature::{RSA_PKCS1_2048_8192_SHA256, RsaPublicKeyComponents};
use serde::Deserialize;
use serde::de::DeserializeOwned;

/// Clock skew tolerated on `exp` and `nbf`, in seconds.
const LEEWAY: u64 = 60;

/// One entry of a JWKS document (`/cdn-cgi/access/certs`).
#[derive(Debug, Clone, Deserialize)]
pub struct Jwk {
    pub kty: String,
    pub kid: Option<String>,
    pub n: Option<String>,
    pub e: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct JwkSet {
    pub keys: Vec<Jwk>,
}

/// An RSA public key: big-endian modulus and exponent.
#[derive(Debug, Clone)]
pub struct RsaKey {
    n: Vec<u8>,
    e: Vec<u8>,
}

impl JwkSet {
    /// The RSA key with this id, if the set has one.
    pub fn rsa_key(&self, kid: &str) -> Option<RsaKey> {
        let jwk = self
            .keys
            .iter()
            .find(|k| k.kty == "RSA" && k.kid.as_deref() == Some(kid))?;
        Some(RsaKey {
            n: URL_SAFE_NO_PAD.decode(jwk.n.as_deref()?).ok()?,
            e: URL_SAFE_NO_PAD.decode(jwk.e.as_deref()?).ok()?,
        })
    }
}

#[derive(Deserialize)]
struct Header {
    alg: String,
    kid: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
struct RegisteredClaims {
    iss: Option<String>,
    aud: Option<Audience>,
    exp: Option<u64>,
    nbf: Option<u64>,
}

struct Parts<'a> {
    header: &'a str,
    payload: &'a str,
    signature: &'a str,
}

fn split(token: &str) -> Result<Parts<'_>, String> {
    let mut it = token.split('.');
    match (it.next(), it.next(), it.next(), it.next()) {
        (Some(header), Some(payload), Some(signature), None) => Ok(Parts {
            header,
            payload,
            signature,
        }),
        _ => Err("malformed token".into()),
    }
}

fn decode_json<T: DeserializeOwned>(segment: &str, what: &str) -> Result<T, String> {
    let bytes = URL_SAFE_NO_PAD
        .decode(segment)
        .map_err(|_| format!("malformed {what}"))?;
    serde_json::from_slice(&bytes).map_err(|_| format!("malformed {what}"))
}

/// Returns the token's key id, refusing any algorithm but RS256.
pub fn key_id(token: &str) -> Result<String, String> {
    let header: Header = decode_json(split(token)?.header, "header")?;
    if header.alg != "RS256" {
        return Err(format!("unexpected algorithm {}", header.alg));
    }
    header.kid.ok_or_else(|| "token has no key id".into())
}

/// Checks the signature and the registered claims, then returns the claims
/// deserialized as `T`.
pub fn verify<T: DeserializeOwned>(
    token: &str,
    key: &RsaKey,
    issuer: &str,
    audience: &str,
    now: u64,
) -> Result<T, String> {
    let parts = split(token)?;
    let header: Header = decode_json(parts.header, "header")?;
    if header.alg != "RS256" {
        return Err(format!("unexpected algorithm {}", header.alg));
    }
    let signature = URL_SAFE_NO_PAD
        .decode(parts.signature)
        .map_err(|_| "malformed signature")?;
    let signed = &token[..parts.header.len() + 1 + parts.payload.len()];
    RsaPublicKeyComponents {
        n: &key.n,
        e: &key.e,
    }
    .verify(&RSA_PKCS1_2048_8192_SHA256, signed.as_bytes(), &signature)
    .map_err(|_| "bad signature")?;

    let claims: RegisteredClaims = decode_json(parts.payload, "claims")?;
    let exp = claims.exp.ok_or("token has no expiry")?;
    if now > exp + LEEWAY {
        return Err("token expired".into());
    }
    if claims.nbf.is_some_and(|nbf| now + LEEWAY < nbf) {
        return Err("token not valid yet".into());
    }
    if claims.iss.as_deref() != Some(issuer) {
        return Err("wrong issuer".into());
    }
    let audience_ok = match claims.aud {
        Some(Audience::One(aud)) => aud == audience,
        Some(Audience::Many(auds)) => auds.iter().any(|a| a == audience),
        None => false,
    };
    if !audience_ok {
        return Err("wrong audience".into());
    }
    decode_json(parts.payload, "claims")
}

#[cfg(test)]
pub mod test_support {
    //! Signs tokens with the test key in `tests/fixtures`.
    use super::*;
    use base64::engine::general_purpose::STANDARD;
    use ring::rand::SystemRandom;
    use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair, RsaPublicKeyComponents};

    pub const KEY_PEM: &str = include_str!("../tests/fixtures/access_test_key.pem");

    pub fn key_pair() -> RsaKeyPair {
        let body: String = KEY_PEM
            .lines()
            .filter(|l| !l.starts_with("-----"))
            .collect();
        RsaKeyPair::from_pkcs8(&STANDARD.decode(body).unwrap()).unwrap()
    }

    pub fn jwks(kid: &str) -> JwkSet {
        let public = RsaPublicKeyComponents::<Vec<u8>>::from(key_pair().public());
        serde_json::from_value(serde_json::json!({ "keys": [{
            "kty": "RSA", "kid": kid, "alg": "RS256", "use": "sig",
            "n": URL_SAFE_NO_PAD.encode(&public.n), "e": URL_SAFE_NO_PAD.encode(&public.e),
        }]}))
        .unwrap()
    }

    pub fn sign(header: serde_json::Value, claims: serde_json::Value) -> String {
        let head = URL_SAFE_NO_PAD.encode(header.to_string());
        let body = URL_SAFE_NO_PAD.encode(claims.to_string());
        let input = format!("{head}.{body}");
        let pair = key_pair();
        let mut sig = vec![0; pair.public().modulus_len()];
        pair.sign(
            &RSA_PKCS1_SHA256,
            &SystemRandom::new(),
            input.as_bytes(),
            &mut sig,
        )
        .unwrap();
        format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig))
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{jwks, sign};
    use super::*;
    use serde_json::{Value, json};

    const ISS: &str = "https://team.cloudflareaccess.com";
    const AUD: &str = "aud-tag";
    const NOW: u64 = 1_800_000_000;

    fn header() -> Value {
        json!({ "alg": "RS256", "kid": "k1", "typ": "JWT" })
    }

    fn claims() -> Value {
        json!({ "iss": ISS, "aud": [AUD], "exp": NOW + 600, "email": "a@example.com" })
    }

    fn check(token: &str) -> Result<Value, String> {
        let key = jwks("k1").rsa_key(&key_id(token)?).ok_or("no key")?;
        verify(token, &key, ISS, AUD, NOW)
    }

    #[test]
    fn accepts_a_valid_token() {
        let claims = check(&sign(header(), claims())).unwrap();
        assert_eq!(claims["email"], "a@example.com");
        // `aud` may also be a single string.
        let mut single = self::claims();
        single["aud"] = json!(AUD);
        assert!(check(&sign(header(), single)).is_ok());
    }

    #[test]
    fn rejects_bad_claims() {
        let cases = [
            ("exp", json!(NOW - LEEWAY - 1), "token expired"),
            ("nbf", json!(NOW + LEEWAY + 1), "token not valid yet"),
            (
                "iss",
                json!("https://evil.cloudflareaccess.com"),
                "wrong issuer",
            ),
            ("aud", json!(["other"]), "wrong audience"),
        ];
        for (field, value, error) in cases {
            let mut c = claims();
            c[field] = value;
            assert_eq!(check(&sign(header(), c)).unwrap_err(), error, "{field}");
        }
        for field in ["exp", "iss", "aud"] {
            let mut c = claims();
            c.as_object_mut().unwrap().remove(field);
            assert!(check(&sign(header(), c)).is_err(), "missing {field}");
        }
    }

    #[test]
    fn rejects_forged_tokens() {
        let token = sign(header(), claims());
        let (signed, _) = token.rsplit_once('.').unwrap();

        // Claims swapped after signing.
        let (head, _) = signed.split_once('.').unwrap();
        let mut forged_claims = claims();
        forged_claims["email"] = json!("intruder@example.com");
        let forged = format!(
            "{head}.{}.{}",
            URL_SAFE_NO_PAD.encode(forged_claims.to_string()),
            token.rsplit_once('.').unwrap().1
        );
        assert_eq!(check(&forged).unwrap_err(), "bad signature");

        // `alg: none` and HMAC headers.
        for alg in ["none", "HS256", "RS512"] {
            let mut h = header();
            h["alg"] = json!(alg);
            let head = URL_SAFE_NO_PAD.encode(h.to_string());
            let body = URL_SAFE_NO_PAD.encode(claims().to_string());
            assert!(check(&format!("{head}.{body}.")).is_err(), "{alg}");
        }

        // Garbage and wrong shapes.
        for bad in ["", "a.b", "a.b.c.d", "!!.!!.!!", &format!("{signed}.")] {
            assert!(check(bad).is_err(), "{bad:?}");
        }

        // Unknown key id.
        let mut h = header();
        h["kid"] = json!("k2");
        assert_eq!(check(&sign(h, claims())).unwrap_err(), "no key");
    }
}
