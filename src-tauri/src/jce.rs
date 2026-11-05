//! 极简 JCE / TARS 编解码，只覆盖虎牙弹幕所需的最小子集。
//!
//! 字段头（tag head）规则与参考实现 \`huya-danmu/lib.js\` 的 Taf 一致：
//! 首字节 = (tag << 4) | type，tag >= 15 时首字节是 0xF0 | type，第二字节才是 tag。
//! 类型编号 0 int8 / 1 int16 / 2 int32 / 3 int64 / 6 string1 / 7 string4 /
//! 8 map / 9 list / 10 struct begin / 11 struct end / 12 zero / 13 simple list。

type Result<T> = std::result::Result<T, String>;

pub const T_INT8: u8 = 0;
pub const T_INT16: u8 = 1;
pub const T_INT32: u8 = 2;
pub const T_INT64: u8 = 3;
pub const T_FLOAT: u8 = 4;
pub const T_DOUBLE: u8 = 5;
pub const T_STRING1: u8 = 6;
pub const T_STRING4: u8 = 7;
pub const T_MAP: u8 = 8;
pub const T_LIST: u8 = 9;
pub const T_STRUCT_BEGIN: u8 = 10;
pub const T_STRUCT_END: u8 = 11;
pub const T_ZERO: u8 = 12;
pub const T_SIMPLE_LIST: u8 = 13;

/// 编码器：按需追加字段，最后取走字节。
#[derive(Default)]
pub struct Enc {
    buf: Vec<u8>,
}

impl Enc {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub fn into_vec(self) -> Vec<u8> {
        self.buf
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.buf
    }

    fn head(&mut self, tag: u8, ty: u8) {
        if tag < 15 {
            self.buf.push((tag << 4) | ty);
        } else {
            self.buf.push(0xF0 | ty);
            self.buf.push(tag);
        }
    }

    /// 整数按最小可用宽度编码，0 走 zero 类型（只占一个头字节）。
    pub fn int(&mut self, tag: u8, value: i64) {
        if value == 0 {
            self.head(tag, T_ZERO);
        } else if (-128..=127).contains(&value) {
            self.head(tag, T_INT8);
            self.buf.push(value as i8 as u8);
        } else if (-32768..=32767).contains(&value) {
            self.head(tag, T_INT16);
            self.buf.extend_from_slice(&(value as i16).to_be_bytes());
        } else if (i32::MIN as i64..=i32::MAX as i64).contains(&value) {
            self.head(tag, T_INT32);
            self.buf.extend_from_slice(&(value as i32).to_be_bytes());
        } else {
            self.head(tag, T_INT64);
            self.buf.extend_from_slice(&value.to_be_bytes());
        }
    }

    pub fn boolean(&mut self, tag: u8, value: bool) {
        self.int(tag, if value { 1 } else { 0 });
    }

    pub fn string(&mut self, tag: u8, value: &str) {
        let bytes = value.as_bytes();
        if bytes.len() > 255 {
            self.head(tag, T_STRING4);
            self.buf.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        } else {
            self.head(tag, T_STRING1);
            self.buf.push(bytes.len() as u8);
        }
        self.buf.extend_from_slice(bytes);
    }

    /// 二进制字段：simple list，元素类型 int8，先写长度再写原始字节。
    pub fn bytes(&mut self, tag: u8, value: &[u8]) {
        self.head(tag, T_SIMPLE_LIST);
        self.head(0, T_INT8);
        self.int(0, value.len() as i64);
        self.buf.extend_from_slice(value);
    }

    pub fn map_bytes(&mut self, tag: u8, items: &[(&str, Vec<u8>)]) {
        self.head(tag, T_MAP);
        self.int(0, items.len() as i64);
        for (key, value) in items {
            self.string(0, key);
            self.bytes(1, value);
        }
    }

    /// 直接拼入已经编码好的字节（用于嵌套结构体）。
    pub fn raw(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    pub fn empty_map(&mut self, tag: u8) {
        self.head(tag, T_MAP);
        self.int(0, 0);
    }

    pub fn begin_struct(&mut self, tag: u8) {
        self.head(tag, T_STRUCT_BEGIN);
    }

    pub fn end_struct(&mut self) {
        self.head(0, T_STRUCT_END);
    }
}

/// 解出来的字段值；只保留弹幕需要的文本、整数、二进制与嵌套结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Int(i64),
    Text(String),
    Bytes(Vec<u8>),
    Struct(Vec<(u8, Value)>),
}

pub fn get_int(fields: &[(u8, Value)], tag: u8) -> Option<i64> {
    fields.iter().find_map(|(t, value)| match (t, value) {
        (t, Value::Int(int)) if *t == tag => Some(*int),
        _ => None,
    })
}

pub fn get_str(fields: &[(u8, Value)], tag: u8) -> Option<&str> {
    fields.iter().find_map(|(t, value)| match (t, value) {
        (t, Value::Text(text)) if *t == tag => Some(text.as_str()),
        _ => None,
    })
}

pub fn get_bytes(fields: &[(u8, Value)], tag: u8) -> Option<&[u8]> {
    fields.iter().find_map(|(t, value)| match (t, value) {
        (t, Value::Bytes(bytes)) if *t == tag => Some(bytes.as_slice()),
        _ => None,
    })
}

pub fn get_struct<'a>(fields: &'a [(u8, Value)], tag: u8) -> Option<&'a [(u8, Value)]> {
    fields.iter().find_map(|(t, value)| match (t, value) {
        (t, Value::Struct(inner)) if *t == tag => Some(inner.as_slice()),
        _ => None,
    })
}

/// 解析一段 JCE 字节流，返回顶层字段表。
pub fn decode(buf: &[u8]) -> Result<Vec<(u8, Value)>> {
    let mut dec = Dec { buf, pos: 0 };
    let mut fields = Vec::new();
    while dec.pos < buf.len() {
        let (tag, ty) = dec.head()?;
        if ty == T_STRUCT_END {
            break;
        }
        let value = dec.value(ty)?;
        if let Some(value) = value {
            fields.push((tag, value));
        }
    }
    Ok(fields)
}

struct Dec<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Dec<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self.pos + len;
        if end > self.buf.len() {
            return Err("JCE 数据提前结束".into());
        }
        let slice = &self.buf[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn head(&mut self) -> Result<(u8, u8)> {
        let first = self.take(1)?[0];
        let ty = first & 0x0F;
        let mut tag = first >> 4;
        if tag == 15 {
            tag = self.take(1)?[0];
        }
        Ok((tag, ty))
    }

    fn int_of(&mut self, ty: u8) -> Result<i64> {
        Ok(match ty {
            T_ZERO => 0,
            T_INT8 => self.take(1)?[0] as i8 as i64,
            T_INT16 => {
                let b = self.take(2)?;
                i16::from_be_bytes([b[0], b[1]]) as i64
            }
            T_INT32 => {
                let b = self.take(4)?;
                i32::from_be_bytes([b[0], b[1], b[2], b[3]]) as i64
            }
            T_INT64 => {
                let b = self.take(8)?;
                i64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
            }
            _ => return Err(format!("不是整数类型：{ty}")),
        })
    }

    /// 集合长度、元素个数这类「先头字节再整数」的计数，编码端会按值域收窄，
    /// 所以读的时候必须先取头字节再按类型读，不能假定固定 4 字节。
    fn counted(&mut self) -> Result<i64> {
        let (_, ty) = self.head()?;
        self.int_of(ty)
    }

    fn string_of(&mut self, ty: u8) -> Result<String> {
        let len = match ty {
            T_STRING1 => self.take(1)?[0] as usize,
            T_STRING4 => {
                let b = self.take(4)?;
                u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize
            }
            _ => return Err(format!("不是字符串类型：{ty}")),
        };
        let bytes = self.take(len)?;
        Ok(String::from_utf8_lossy(bytes).into_owned())
    }

    /// 返回 None 表示该字段类型不关心，但字节已经跳过。
    fn value(&mut self, ty: u8) -> Result<Option<Value>> {
        match ty {
            T_ZERO | T_INT8 | T_INT16 | T_INT32 | T_INT64 => Ok(Some(Value::Int(self.int_of(ty)?))),
            T_FLOAT => {
                self.take(4)?;
                Ok(None)
            }
            T_DOUBLE => {
                self.take(8)?;
                Ok(None)
            }
            T_STRING1 | T_STRING4 => Ok(Some(Value::Text(self.string_of(ty)?))),
            T_STRUCT_BEGIN => {
                let mut fields = Vec::new();
                loop {
                    let (tag, inner) = self.head()?;
                    if inner == T_STRUCT_END {
                        break;
                    }
                    if let Some(value) = self.value(inner)? {
                        fields.push((tag, value));
                    }
                }
                Ok(Some(Value::Struct(fields)))
            }
            T_SIMPLE_LIST => {
                let (_, inner) = self.head()?;
                if inner != T_INT8 {
                    return Err(format!("simple list 元素类型不是 int8：{inner}"));
                }
                let len = self.counted()?;
                if len < 0 {
                    return Err("simple list 长度为负".into());
                }
                Ok(Some(Value::Bytes(self.take(len as usize)?.to_vec())))
            }
            T_LIST => {
                let size = self.counted()?;
                for _ in 0..size.max(0) {
                    let (_, inner) = self.head()?;
                    self.value(inner)?;
                }
                Ok(None)
            }
            T_MAP => {
                let size = self.counted()?;
                for _ in 0..size.max(0) {
                    let (_, key_ty) = self.head()?;
                    self.value(key_ty)?;
                    let (_, value_ty) = self.head()?;
                    self.value(value_ty)?;
                }
                Ok(None)
            }
            other => Err(format!("不支持的 JCE 类型：{other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_int_uses_narrow_encoding() {
        let mut enc = Enc::new();
        enc.int(0, 3);
        assert_eq!(enc.into_vec(), vec![0x00, 0x03]);

        let mut enc = Enc::new();
        enc.int(0, 0);
        assert_eq!(enc.into_vec(), vec![0x0C]);

        let mut enc = Enc::new();
        enc.int(0, 1346609715);
        assert_eq!(enc.into_vec(), vec![0x02, 0x50, 0x43, 0xA2, 0x33]);
    }

    #[test]
    fn strings_switch_to_long_form() {
        let mut enc = Enc::new();
        enc.string(1, "abc");
        assert_eq!(enc.into_vec(), vec![0x16, 0x03, b'a', b'b', b'c']);

        let long = "x".repeat(300);
        let mut enc = Enc::new();
        enc.string(1, &long);
        let bytes = enc.into_vec();
        assert_eq!(bytes[0], 0x17);
        assert_eq!(&bytes[1..5], &300u32.to_be_bytes());
        assert_eq!(bytes.len(), 5 + 300);
    }

    #[test]
    fn bytes_are_simple_lists() {
        let mut enc = Enc::new();
        enc.bytes(1, &[0xAA, 0xBB]);
        assert_eq!(enc.into_vec(), vec![0x1D, 0x00, 0x00, 0x02, 0xAA, 0xBB]);
    }

    #[test]
    fn decode_round_trips_struct_with_nested_fields() {
        let mut inner = Enc::new();
        inner.int(0, 42);
        inner.string(2, "昵称");
        inner.end_struct();
        let mut outer = Enc::new();
        outer.begin_struct(0);
        outer.buf.extend_from_slice(inner.as_slice());
        outer.int(1, 7);
        outer.end_struct();

        let fields = decode(outer.as_slice()).expect("应该解析成功");
        let nested = get_struct(&fields, 0).expect("应该有嵌套结构");
        assert_eq!(get_int(nested, 0), Some(42));
        assert_eq!(get_str(nested, 2), Some("昵称"));
        assert_eq!(get_int(&fields, 1), Some(7));
    }

    #[test]
    fn decode_skips_unknown_shapes() {
        // map 与非 int8 的 list 都要能安全跳过，不能打乱后续字段。
        let mut enc = Enc::new();
        enc.head(0, T_MAP);
        enc.int(0, 1);
        enc.string(0, "k");
        enc.string(1, "v");
        enc.int(3, 9);
        let fields = decode(enc.as_slice()).expect("应该解析成功");
        assert_eq!(get_int(&fields, 3), Some(9));
    }
}
