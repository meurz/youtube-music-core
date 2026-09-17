//! Bounded protobuf and UMP wire decoding. See THIRD_PARTY_NOTICES.md.
use crate::{Error, Result};

pub(super) fn bad() -> Error {
    Error::Protocol("malformed or oversized SABR message".into())
}

pub(super) fn varint(mut v: u64, out: &mut Vec<u8>) {
    while v >= 128 {
        out.push((v as u8) | 128);
        v >>= 7;
    }
    out.push(v as u8);
}
pub(super) fn uint(n: u32, v: u64, out: &mut Vec<u8>) {
    varint(u64::from(n) << 3, out);
    varint(v, out);
}
pub(super) fn bytes(n: u32, v: &[u8], out: &mut Vec<u8>) {
    varint((u64::from(n) << 3) | 2, out);
    varint(v.len() as u64, out);
    out.extend_from_slice(v);
}
pub(super) fn float(n: u32, v: f32, out: &mut Vec<u8>) {
    varint((u64::from(n) << 3) | 5, out);
    out.extend_from_slice(&v.to_le_bytes());
}
fn read_varint(data: &[u8], pos: &mut usize) -> Result<u64> {
    let mut value = 0;
    for i in 0..10 {
        let b = *data.get(*pos).ok_or_else(bad)?;
        *pos += 1;
        if i == 9 && b > 1 {
            return Err(bad());
        }
        value |= u64::from(b & 127) << (7 * i);
        if b < 128 {
            return Ok(value);
        }
    }
    Err(bad())
}
#[derive(Clone, Copy)]
pub(super) enum Value<'a> {
    Uint(u64),
    Bytes(&'a [u8]),
    Fixed,
}
pub(super) struct Message<'a>(Vec<(u32, Value<'a>)>);
impl<'a> Message<'a> {
    pub(super) fn parse(data: &'a [u8]) -> Result<Self> {
        if data.len() > 262144 {
            return Err(bad());
        }
        let mut p = 0;
        let mut fields = Vec::new();
        while p < data.len() {
            if fields.len() >= 4096 {
                return Err(bad());
            }
            let key = read_varint(data, &mut p)?;
            if key >> 3 == 0 || key >> 3 > 0x1fff_ffff {
                return Err(bad());
            }
            let v = match key & 7 {
                0 => Value::Uint(read_varint(data, &mut p)?),
                2 => {
                    let len = usize::try_from(read_varint(data, &mut p)?).map_err(|_| bad())?;
                    let end = p.checked_add(len).ok_or_else(bad)?;
                    let bytes = data.get(p..end).ok_or_else(bad)?;
                    p = end;
                    Value::Bytes(bytes)
                }
                1 | 5 => {
                    p = p
                        .checked_add(if key & 7 == 1 { 8 } else { 4 })
                        .ok_or_else(bad)?;
                    if p > data.len() {
                        return Err(bad());
                    }
                    Value::Fixed
                }
                _ => return Err(bad()),
            };
            fields.push(((key >> 3) as u32, v));
        }
        Ok(Self(fields))
    }
    pub(super) fn uint(&self, field: u32) -> Result<Option<u64>> {
        let mut found = None;
        for (n, v) in &self.0 {
            if *n == field {
                match v {
                    Value::Uint(v) if found.is_none() => found = Some(*v),
                    _ => return Err(bad()),
                }
            }
        }
        Ok(found)
    }
    pub(super) fn bytes(&self, field: u32) -> Result<Option<&'a [u8]>> {
        let mut found = None;
        for (n, v) in &self.0 {
            if *n == field {
                match v {
                    Value::Bytes(v) if found.is_none() => found = Some(*v),
                    _ => return Err(bad()),
                }
            }
        }
        Ok(found)
    }
    pub(super) fn string(&self, field: u32) -> Result<Option<&'a str>> {
        self.bytes(field)?
            .map(|v| std::str::from_utf8(v).map_err(|_| bad()))
            .transpose()
    }
    pub(super) fn repeated_uint(&self, field: u32) -> Result<Vec<u64>> {
        let mut result = Vec::new();
        for (n, v) in &self.0 {
            if *n == field {
                match v {
                    Value::Uint(v) => result.push(*v),
                    Value::Bytes(v) => {
                        let mut p = 0;
                        while p < v.len() {
                            result.push(read_varint(v, &mut p)?);
                            if result.len() > 256 {
                                return Err(bad());
                            }
                        }
                    }
                    _ => return Err(bad()),
                }
            }
        }
        if result.len() > 256 {
            return Err(bad());
        }
        Ok(result)
    }
}

pub(super) fn ump_uint(data: &[u8], pos: &mut usize) -> Result<u32> {
    let b = *data.get(*pos).ok_or_else(bad)?;
    let len = if b < 128 {
        1
    } else if b < 192 {
        2
    } else if b < 224 {
        3
    } else if b < 240 {
        4
    } else {
        5
    };
    let end = pos.checked_add(len).ok_or_else(bad)?;
    let x = data.get(*pos..end).ok_or_else(bad)?;
    *pos = end;
    Ok(match len {
        1 => u32::from(b),
        2 => u32::from(b & 63) | (u32::from(x[1]) << 6),
        3 => u32::from(b & 31) | (u32::from(x[1]) << 5) | (u32::from(x[2]) << 13),
        4 => {
            u32::from(b & 15)
                | (u32::from(x[1]) << 4)
                | (u32::from(x[2]) << 12)
                | (u32::from(x[3]) << 20)
        }
        _ => u32::from_le_bytes(x[1..5].try_into().map_err(|_| bad())?),
    })
}
pub(super) fn parts(data: &[u8]) -> Result<Vec<(u32, &[u8])>> {
    let mut p = 0;
    let mut out = Vec::new();
    while p < data.len() {
        crate::operation::check()?;
        if out.len() >= 16384 {
            return Err(bad());
        }
        let kind = ump_uint(data, &mut p)?;
        let len = ump_uint(data, &mut p)? as usize;
        if len > super::MAX_SEGMENT {
            return Err(bad());
        }
        let end = p.checked_add(len).ok_or_else(bad)?;
        out.push((kind, data.get(p..end).ok_or_else(bad)?));
        p = end;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ump_lengths_and_truncation() {
        for (encoded, want) in [
            (vec![127], 127),
            (vec![128, 2], 128),
            (vec![192, 0, 2], 16384),
            (vec![224, 0, 0, 2], 2097152),
            (vec![240, 255, 255, 255, 255], u32::MAX),
        ] {
            assert_eq!(ump_uint(&encoded, &mut 0).unwrap(), want);
            for end in 0..encoded.len() {
                assert!(ump_uint(&encoded[..end], &mut 0).is_err());
            }
        }
        assert!(parts(&[20, 4, 0]).is_err());
        assert!(parts(&[20, 240, 255, 255, 255, 255]).is_err());
    }
    #[test]
    fn protobuf_unknown_fixed_fields_and_overflow() {
        let mut b = Vec::new();
        uint(1, 123, &mut b);
        bytes(7, b"hello", &mut b);
        float(9, 1., &mut b);
        let m = Message::parse(&b).unwrap();
        assert_eq!(m.uint(1).unwrap(), Some(123));
        assert_eq!(m.string(7).unwrap(), Some("hello"));
        assert!(Message::parse(&[8, 255, 255, 255, 255, 255, 255, 255, 255, 255, 2]).is_err());
        assert!(Message::parse(&[0]).is_err());
        assert!(Message::parse(&[10, 128]).is_err());
        assert!(Message::parse(&[8, 1, 8, 2]).unwrap().uint(1).is_err());
    }
}
