//! 证书有效期解析（**最小 DER 行走**，只为读 notBefore/notAfter）。
//!
//! 为什么自己写：我们只需要 X.509 的 Validity 两段时间，引入完整 X.509 解析器代价过大；
//! 但**必须严格**——解析失败一律返回 None，绝不用“猜一个合理默认值”掩盖。
//!
//! 支持 `UTCTime`（YYMMDDHHMMSSZ，RFC 5280：>=50 视为 19xx）与 `GeneralizedTime`（YYYYMMDDHHMMSSZ）。

/// 证书有效期（Unix 秒）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CertificateValidity {
    /// notBefore（Unix 秒）。
    pub not_before_unix: i64,
    /// notAfter（Unix 秒）。
    pub not_after_unix: i64,
}

impl CertificateValidity {
    /// 距离到期还有多少天（负数 = 已过期）。按 86400 秒向下取整。
    pub fn days_remaining(&self, now_unix: i64) -> i64 {
        self.not_after_unix.saturating_sub(now_unix).div_euclid(86_400)
    }

    /// 是否已过期。
    pub fn is_expired(&self, now_unix: i64) -> bool {
        now_unix >= self.not_after_unix
    }
}

/// 从 **DER** 证书字节解析有效期；任何结构不符返回 None。
pub fn parse_der(der: &[u8]) -> Option<CertificateValidity> {
    let mut reader = Reader::new(der);
    // Certificate ::= SEQUENCE { tbsCertificate, signatureAlgorithm, signature }
    let certificate = reader.expect_tag(0x30)?;
    let mut certificate = Reader::new(certificate);
    // TBSCertificate ::= SEQUENCE { ... }
    let tbs = certificate.expect_tag(0x30)?;
    let mut tbs = Reader::new(tbs);
    // [0] version（可选）
    if tbs.peek_tag() == Some(0xA0) {
        tbs.expect_tag(0xA0)?;
    }
    // serialNumber
    tbs.expect_tag(0x02)?;
    // signature AlgorithmIdentifier
    tbs.expect_tag(0x30)?;
    // issuer Name
    tbs.expect_tag(0x30)?;
    // validity SEQUENCE { notBefore, notAfter }
    let validity = tbs.expect_tag(0x30)?;
    let mut validity = Reader::new(validity);
    let not_before = parse_time(&mut validity)?;
    let not_after = parse_time(&mut validity)?;
    if not_after <= not_before {
        return None;
    }
    Some(CertificateValidity {
        not_before_unix: not_before,
        not_after_unix: not_after,
    })
}

fn parse_time(reader: &mut Reader<'_>) -> Option<i64> {
    match reader.peek_tag()? {
        0x17 => {
            let text = std::str::from_utf8(reader.expect_tag(0x17)?).ok()?;
            parse_utc_time(text)
        }
        0x18 => {
            let text = std::str::from_utf8(reader.expect_tag(0x18)?).ok()?;
            parse_generalized_time(text)
        }
        _ => None,
    }
}

fn parse_two(text: &[u8], at: usize) -> Option<i64> {
    let slice = text.get(at..at + 2)?;
    let value = std::str::from_utf8(slice).ok()?;
    value.parse::<i64>().ok()
}

fn parse_utc_time(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() != 13 || bytes[12] != b'Z' {
        return None;
    }
    let year = parse_two(bytes, 0)?;
    // RFC 5280：00..=49 -> 20xx，50..=99 -> 19xx。
    let full_year = if year >= 50 { 1900 + year } else { 2000 + year };
    let month = parse_two(bytes, 2)?;
    let day = parse_two(bytes, 4)?;
    let hour = parse_two(bytes, 6)?;
    let minute = parse_two(bytes, 8)?;
    let second = parse_two(bytes, 10)?;
    to_unix(full_year, month, day, hour, minute, second)
}

fn parse_generalized_time(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() != 15 || bytes[14] != b'Z' {
        return None;
    }
    let year: i64 = std::str::from_utf8(bytes.get(0..4)?).ok()?.parse().ok()?;
    let month = parse_two(bytes, 4)?;
    let day = parse_two(bytes, 6)?;
    let hour = parse_two(bytes, 8)?;
    let minute = parse_two(bytes, 10)?;
    let second = parse_two(bytes, 12)?;
    to_unix(year, month, day, hour, minute, second)
}

/// 公历 -> Unix 秒（霍华德·欣南特算法；不依赖 chrono）。
fn to_unix(year: i64, month: i64, day: i64, hour: i64, minute: i64, second: i64) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if !(0..=23).contains(&hour) || !(0..=59).contains(&minute) || !(0..=60).contains(&second) {
        return None;
    }
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// 极简 TLV 读取器（只支持短格式长度；证书里长度通常 < 128 或两字节）。
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn peek_tag(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    /// 读一个 TLV，返回内容切片。
    fn expect_tag(&mut self, tag: u8) -> Option<&'a [u8]> {
        if self.peek_tag()? != tag {
            return None;
        }
        self.at += 1;
        let first = *self.bytes.get(self.at)?;
        self.at += 1;
        let length = if first & 0x80 == 0 {
            first as usize
        } else {
            let count = (first & 0x7f) as usize;
            if count == 0 || count > 3 {
                return None;
            }
            let mut value = 0usize;
            for _ in 0..count {
                value = (value << 8) | *self.bytes.get(self.at)? as usize;
                self.at += 1;
            }
            value
        };
        let end = self.at.checked_add(length)?;
        let slice = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_repository_test_certificate() {
        let pem = include_bytes!("../../../crates/node-client/tests/fixtures/tls/test-cert.pem");
        let der = pem_to_der(pem).expect("pem must contain a certificate");
        let validity = parse_der(&der).expect("validity must parse");
        // 夹具证书 10 年有效期：notAfter 必须显著晚于 notBefore。
        assert!(validity.not_after_unix > validity.not_before_unix);
        // 2020-01-01 之后签发（夹具是 2026 年生成的）。
        assert!(validity.not_before_unix > 1_577_836_800);
        // 剩余天数为正且小于 20 年。
        let days = validity.days_remaining(validity.not_before_unix);
        assert!(days > 0 && days < 365 * 20, "unexpected days: {days}");
    }

    #[test]
    fn rejects_truncated_or_garbage_input() {
        assert!(parse_der(&[]).is_none());
        assert!(parse_der(&[0x30, 0x00]).is_none());
        assert!(parse_der(&[0x02, 0x01, 0x00]).is_none());
        // 长度超过剩余字节 -> None（不 panic）。
        assert!(parse_der(&[0x30, 0x7f, 0x00]).is_none());
    }

    #[test]
    fn day_math_and_expiry_checks() {
        let validity = CertificateValidity {
            not_before_unix: 1_700_000_000,
            not_after_unix: 1_700_000_000 + 86_400 * 30,
        };
        assert_eq!(validity.days_remaining(1_700_000_000), 30);
        assert_eq!(validity.days_remaining(1_700_000_000 + 86_400 * 29), 1);
        assert_eq!(validity.days_remaining(1_700_000_000 + 86_400 * 30), 0);
        assert_eq!(validity.days_remaining(1_700_000_000 + 86_400 * 31), -1);
        assert!(!validity.is_expired(1_700_000_000));
        assert!(validity.is_expired(1_700_000_000 + 86_400 * 30));
    }

    fn pem_to_der(pem: &[u8]) -> Option<Vec<u8>> {
        let text = std::str::from_utf8(pem).ok()?;
        let begin = "-----BEGIN CERTIFICATE-----";
        let end = "-----END CERTIFICATE-----";
        let start = text.find(begin)? + begin.len();
        let stop = text[start..].find(end)? + start;
        let body: String = text[start..stop].chars().filter(|c| !c.is_whitespace()).collect();
        let mut out = Vec::with_capacity(body.len() * 3 / 4);
        let mut buffer: u32 = 0;
        let mut bits = 0u32;
        const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        for byte in body.bytes() {
            if byte == b'=' {
                break;
            }
            let index = TABLE.iter().position(|c| *c == byte)? as u32;
            buffer = (buffer << 6) | index;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((buffer >> bits) as u8);
            }
        }
        Some(out)
    }
}
