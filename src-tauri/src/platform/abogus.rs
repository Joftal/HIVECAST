//! 抖音 ABogus 签名。
//!
//! 抖音的 webcast 接口要求 `a_bogus` 查询参数，它由「参数串 + body + UA + 浏览器指纹 + 时间戳」
//! 经过 SM3、RC4、自定义字母表 base64 与一张 256 字节置换表的异或混合得到。算法对齐
//! biliLive-tools/packages/DouYinRecorder/src/sign.ts（其源自 rust-srec 的 abogus.rs），
//! 见 docs/25 §2。
//!
//! 这里刻意不引第三方 crate：SM3 手写 60 行，避免为签名再拉一条依赖链。

const SALT: &str = "cus";

/// a_bogus 输出用的字母表
const ALPHA_ABOGUS: &[u8; 64] = b"Dkdpgh2ZmsQB80/MfvV36XI1R45-WUAlEixNLwoqYTOPuzKFjJnry79HbGcaStCe";
/// UA 摘要前先做一次 base64，用另一套字母表
const ALPHA_BASE64: &[u8; 64] = b"ckdp1h4ZKsUB80/Mfvw36XIgR25+WQAlEi7NLboqYTOPuzmFjJnryx9HVGDaStCe";

/// 256 字节置换表；transform 时会被就地交换，所以每次签名都从副本开始。
const TABLE: [u8; 256] = [
    121, 243, 55, 234, 103, 36, 47, 228, 30, 231, 106, 6, 115, 95, 78, 101, 250, 207, 198, 50, 139,
    227, 220, 105, 97, 143, 34, 28, 194, 215, 18, 100, 159, 160, 43, 8, 169, 217, 180, 120, 247,
    45, 90, 11, 27, 197, 46, 3, 84, 72, 5, 68, 62, 56, 221, 75, 144, 79, 73, 161, 178, 81, 64, 187,
    134, 117, 186, 118, 16, 241, 130, 71, 89, 147, 122, 129, 65, 40, 88, 150, 110, 219, 199, 255,
    181, 254, 48, 4, 195, 248, 208, 32, 116, 167, 69, 201, 17, 124, 125, 104, 96, 83, 80, 127, 236,
    108, 154, 126, 204, 15, 20, 135, 112, 158, 13, 1, 188, 164, 210, 237, 222, 98, 212, 77, 253,
    42, 170, 202, 26, 22, 29, 182, 251, 10, 173, 152, 58, 138, 54, 141, 185, 33, 157, 31, 252, 132,
    233, 235, 102, 196, 191, 223, 240, 148, 39, 123, 92, 82, 128, 109, 57, 24, 38, 113, 209, 245,
    2, 119, 153, 229, 189, 214, 230, 174, 232, 63, 52, 205, 86, 140, 66, 175, 111, 171, 246, 133,
    238, 193, 99, 60, 74, 91, 225, 51, 76, 37, 145, 211, 166, 151, 213, 206, 0, 200, 244, 176, 218,
    44, 184, 172, 49, 216, 93, 168, 53, 21, 183, 41, 67, 85, 224, 155, 226, 242, 87, 177, 146, 70,
    190, 12, 162, 19, 137, 114, 25, 165, 163, 192, 23, 59, 9, 94, 179, 107, 35, 7, 142, 131, 239,
    203, 149, 136, 61, 249, 14, 156,
];

const SORT1: [usize; 44] = [
    18, 20, 52, 26, 30, 34, 58, 38, 40, 53, 42, 21, 27, 54, 55, 31, 35, 57, 39, 41, 43, 22, 28, 32,
    60, 36, 23, 29, 33, 37, 44, 45, 59, 46, 47, 48, 49, 50, 24, 25, 65, 66, 70, 71,
];

const SORT2: [usize; 44] = [
    18, 20, 26, 30, 34, 38, 40, 42, 21, 27, 31, 35, 39, 41, 43, 22, 28, 32, 36, 23, 29, 33, 37, 44,
    45, 46, 47, 48, 49, 50, 24, 25, 52, 53, 54, 55, 57, 58, 59, 60, 65, 66, 70, 71,
];

/// 签名用的 UA。抖音把 UA 摘要写进 a_bogus，所以请求头必须与这里完全一致。
pub const SIGN_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36 Edg/130.0.0.0";

/// 浏览器指纹：屏幕/窗口/色彩/时区/平台。取值只要自洽即可，服务端不校验真伪。
pub const SIGN_FP: &str = "1920|1080|1944|1170|0|30|0|0|1920|1080|1920|1080|1920|1080|24|24|Win32";

/// 签名上下文。测试里可以固定时间戳与随机前缀，从而得到可复现的向量。
pub struct SignOptions<'a> {
    pub ua: &'a str,
    pub fp: &'a str,
    pub options: [i32; 3],
    /// 生成开始时间（毫秒）
    pub start_ms: i64,
    /// 生成结束时间（毫秒），通常同 start_ms
    pub end_ms: i64,
    /// 12 字节随机前缀
    pub prefix: [u8; 12],
}

impl<'a> SignOptions<'a> {
    pub fn now() -> Self {
        let now = chrono::Utc::now().timestamp_millis();
        Self {
            ua: SIGN_UA,
            fp: SIGN_FP,
            options: [0, 1, 14],
            start_ms: now,
            end_ms: now,
            prefix: random_prefix(),
        }
    }
}

fn random_prefix() -> [u8; 12] {
    // 不需要密码学强度：前缀只用来让每次签名互不相同。
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9e37_79b9_7f4a_7c15)
        | 1;
    // 12 字节前缀不是随便取的：浏览器把 3 个 0..10000 的随机数按固定位掩码摊成 12 字节，
    // 每个字节的低位组合都是固定的（实测把前缀全写成 0 会被服务端判成机器人，返回空响应）。
    let mut out = [0u8; 12];
    for chunk in out.chunks_mut(4) {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let rd = ((seed >> 24) as u16) % 10_000;
        let low = (rd & 0xff) as u8;
        let high = (rd >> 8) as u8;
        chunk[0] = (low & 170) | 1;
        chunk[1] = (low & 85) | 2;
        chunk[2] = (high & 170) | 5;
        chunk[3] = (high & 85) | 40;
    }
    out
}

/// 生成 `a_bogus` 并拼到参数串后面。params 必须是已经编码好的查询串。
pub fn sign(params: &str, body: &str, opts: &SignOptions) -> String {
    format!("{params}&a_bogus={}", raw(params, body, opts))
}

/// 只生成 `a_bogus` 的值。
pub fn raw(params: &str, body: &str, opts: &SignOptions) -> String {
    let arr1 = sm3_digest(&sm3_digest(salted(params).as_bytes()));
    let arr2 = sm3_digest(&sm3_digest(salted(body).as_bytes()));
    let ua_cipher = rc4(&[0x00, 0x01, 0x0e], opts.ua.as_bytes());
    let ua_b64 = base64_encode(&ua_cipher, ALPHA_BASE64);
    let arr3 = sm3_digest(ua_b64.as_bytes());

    let start = opts.start_ms;
    let end = opts.end_ms;
    let options = opts.options;

    let mut dir = [0i32; 72];
    dir[8] = 3;
    dir[18] = 44;
    // 时间戳按字节拆开，加 pageId/aid 等常量
    dir[20] = ((start >> 24) & 0xff) as i32;
    dir[21] = ((start >> 16) & 0xff) as i32;
    dir[22] = ((start >> 8) & 0xff) as i32;
    dir[23] = (start & 0xff) as i32;
    dir[24] = (start / 0x1_0000_0000) as i32;
    dir[25] = (start / 0x100_0000_0000) as i32;
    dir[26] = ((options[0] >> 24) & 0xff) as i32;
    dir[27] = ((options[0] >> 16) & 0xff) as i32;
    dir[28] = ((options[0] >> 8) & 0xff) as i32;
    dir[29] = (options[0] & 0xff) as i32;
    dir[30] = ((options[1] / 256) & 0xff) as i32;
    dir[31] = options[1] % 256;
    dir[32] = ((options[1] >> 24) & 0xff) as i32;
    dir[33] = ((options[1] >> 16) & 0xff) as i32;
    dir[34] = ((options[2] >> 24) & 0xff) as i32;
    dir[35] = ((options[2] >> 16) & 0xff) as i32;
    dir[36] = ((options[2] >> 8) & 0xff) as i32;
    dir[37] = (options[2] & 0xff) as i32;
    dir[38] = arr1[21] as i32;
    dir[39] = arr1[22] as i32;
    dir[40] = arr2[21] as i32;
    dir[41] = arr2[22] as i32;
    dir[42] = arr3[23] as i32;
    dir[43] = arr3[24] as i32;
    dir[44] = ((end >> 24) & 0xff) as i32;
    dir[45] = ((end >> 16) & 0xff) as i32;
    dir[46] = ((end >> 8) & 0xff) as i32;
    dir[47] = (end & 0xff) as i32;
    dir[48] = dir[8];
    dir[49] = (end / 0x1_0000_0000) as i32;
    dir[50] = (end / 0x100_0000_0000) as i32;
    dir[56] = 6383;
    dir[57] = 6383 & 0xff;
    dir[58] = (6383 >> 8) & 0xff;
    dir[59] = (6383 >> 16) & 0xff;
    dir[60] = (6383 >> 24) & 0xff;
    dir[64] = opts.fp.len() as i32;
    dir[65] = opts.fp.len() as i32;

    let mut values: Vec<i32> = SORT1.iter().map(|index| dir[*index]).collect();
    values.extend(opts.fp.bytes().map(i32::from));
    let mut checksum = 0;
    for (index, key) in SORT2.iter().enumerate() {
        checksum = if index == 0 {
            dir[*key]
        } else {
            checksum ^ dir[*key]
        };
    }
    values.push(checksum);

    let transformed = transform_bytes(&values);
    let mut final_bytes: Vec<i32> = opts.prefix.iter().map(|byte| i32::from(*byte)).collect();
    final_bytes.extend(transformed);
    abogus_encode(&final_bytes, ALPHA_ABOGUS)
}

fn salted(value: &str) -> String {
    format!("{value}{SALT}")
}

/// 有状态异或置换：表会被就地交换，因此每次签名都传入新副本。
fn transform_bytes(values: &[i32]) -> Vec<i32> {
    let mut table = TABLE;
    let len = table.len() as i32;
    let mut index_b = i32::from(table[1]);
    let mut initial = 0i32;
    let mut value_e = 0i32;
    let mut out = Vec::with_capacity(values.len());

    for (index, value) in values.iter().enumerate() {
        let sum_initial = if index == 0 {
            initial = i32::from(table[index_b as usize]);
            let sum = index_b + initial;
            table[1] = initial as u8;
            table[index_b as usize] = index_b as u8;
            sum
        } else {
            initial + value_e
        };

        let sum_index = (sum_initial % len) as usize;
        out.push(value ^ i32::from(table[sum_index]));

        let next_index = (index as i32 + 2) % len;
        value_e = i32::from(table[next_index as usize]);
        let new_index = (index_b + value_e) % len;
        initial = i32::from(table[new_index as usize]);
        table.swap(new_index as usize, next_index as usize);
        index_b = new_index;
    }

    out
}

fn abogus_encode(values: &[i32], alphabet: &[u8; 64]) -> String {
    let mut out = String::with_capacity(values.len() * 4 / 3 + 4);
    let mut index = 0;
    while index < values.len() {
        let v1 = values[index];
        let v2 = *values.get(index + 1).unwrap_or(&0);
        let v3 = *values.get(index + 2).unwrap_or(&0);
        // 与 JS 的 32 位按位运算对齐：溢出按回绕处理
        let n = v1.wrapping_shl(16) | v2.wrapping_shl(8) | v3;
        out.push(alphabet[((n & 0xfc_0000) >> 18) as usize] as char);
        out.push(alphabet[((n & 0x03_f000) >> 12) as usize] as char);
        if index + 1 < values.len() {
            out.push(alphabet[((n & 0x0f_c0) >> 6) as usize] as char);
        }
        if index + 2 < values.len() {
            out.push(alphabet[(n & 0x3f) as usize] as char);
        }
        index += 3;
    }
    while out.len() % 4 != 0 {
        out.push('=');
    }
    out
}

fn base64_encode(bytes: &[u8], alphabet: &[u8; 64]) -> String {
    let mut out = String::with_capacity(bytes.len() * 4 / 3 + 4);
    let mut index = 0;
    while index < bytes.len() {
        let b1 = u32::from(bytes[index]);
        let b2 = u32::from(*bytes.get(index + 1).unwrap_or(&0));
        let b3 = u32::from(*bytes.get(index + 2).unwrap_or(&0));
        let n = (b1 << 16) | (b2 << 8) | b3;
        out.push(alphabet[((n >> 18) & 63) as usize] as char);
        out.push(alphabet[((n >> 12) & 63) as usize] as char);
        if index + 1 < bytes.len() {
            out.push(alphabet[((n >> 6) & 63) as usize] as char);
        }
        if index + 2 < bytes.len() {
            out.push(alphabet[(n & 63) as usize] as char);
        }
        index += 3;
    }
    while out.len() % 4 != 0 {
        out.push('=');
    }
    out
}

fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut state: Vec<u8> = (0..=255u8).collect();
    let mut j = 0usize;
    for i in 0..256 {
        j = (j + usize::from(state[i]) + usize::from(key[i % key.len()])) & 0xff;
        state.swap(i, j);
    }

    let mut i = 0usize;
    j = 0;
    let mut out = Vec::with_capacity(data.len());
    for byte in data {
        i = (i + 1) & 0xff;
        j = (j + usize::from(state[i])) & 0xff;
        state.swap(i, j);
        let k = state[(usize::from(state[i]) + usize::from(state[j])) & 0xff];
        out.push(byte ^ k);
    }
    out
}

fn p0(x: u32) -> u32 {
    x ^ x.rotate_left(9) ^ x.rotate_left(17)
}

fn p1(x: u32) -> u32 {
    x ^ x.rotate_left(15) ^ x.rotate_left(23)
}

/// GB/T 32905 的 SM3。node 侧用 `crypto.createHash("sm3")`，这里手写以便零依赖。
pub fn sm3_digest(data: &[u8]) -> [u8; 32] {
    let mut state: [u32; 8] = [
        0x7380_166f,
        0x4914_b2b9,
        0x1724_42d7,
        0xda8a_0600,
        0xa96f_30bc,
        0x1631_38aa,
        0xe38d_ee4d,
        0xb0fb_0e4e,
    ];

    let mut message = data.to_vec();
    let bit_len = (data.len() as u64) * 8;
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in message.chunks(64) {
        let mut w = [0u32; 68];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for j in 16..68 {
            w[j] = p1(w[j - 16] ^ w[j - 9] ^ w[j - 3].rotate_left(15))
                ^ w[j - 13].rotate_left(7)
                ^ w[j - 6];
        }
        let mut w1 = [0u32; 64];
        for j in 0..64 {
            w1[j] = w[j] ^ w[j + 4];
        }

        let (mut a, mut b, mut c, mut d) = (state[0], state[1], state[2], state[3]);
        let (mut e, mut f, mut g, mut h) = (state[4], state[5], state[6], state[7]);

        for j in 0..64 {
            let t: u32 = if j < 16 { 0x79cc_4519 } else { 0x7a87_9d8a };
            let ss1 = a
                .rotate_left(12)
                .wrapping_add(e)
                .wrapping_add(t.rotate_left((j % 32) as u32))
                .rotate_left(7);
            let ss2 = ss1 ^ a.rotate_left(12);
            let ff = if j < 16 {
                a ^ b ^ c
            } else {
                (a & b) | (a & c) | (b & c)
            };
            let gg = if j < 16 {
                e ^ f ^ g
            } else {
                (e & f) | (!e & g)
            };
            let tt1 = ff.wrapping_add(d).wrapping_add(ss2).wrapping_add(w1[j]);
            let tt2 = gg.wrapping_add(h).wrapping_add(ss1).wrapping_add(w[j]);

            d = c;
            c = b.rotate_left(9);
            b = a;
            a = tt1;
            h = g;
            g = f.rotate_left(19);
            f = e;
            e = p0(tt2);
        }

        state[0] ^= a;
        state[1] ^= b;
        state[2] ^= c;
        state[3] ^= d;
        state[4] ^= e;
        state[5] ^= f;
        state[6] ^= g;
        state[7] ^= h;
    }

    let mut out = [0u8; 32];
    for (index, word) in state.iter().enumerate() {
        out[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(
        start: i64,
        end: i64,
        prefix: [u8; 12],
        ua: &'static str,
        fp: &'static str,
    ) -> SignOptions<'static> {
        SignOptions {
            ua,
            fp,
            options: [0, 1, 14],
            start_ms: start,
            end_ms: end,
            prefix,
        }
    }

    /// 向量由 node 版实现（/tmp/ab.mjs）生成，两边必须逐字节一致。
    #[test]
    fn matches_reference_vectors() {
        let cases = [
            (
                "aid=6383&live_id=1",
                "",
                options(
                    1_730_000_000_000,
                    1_730_000_000_000,
                    [7, 11, 23, 101, 102, 103, 204, 205, 206, 1, 2, 3],
                    SIGN_UA,
                    SIGN_FP,
                ),
                "kju14I4qr8H/Dfmp56xLfY3q6X13YQic0SVkMD2fmxfPbL39HMYD9exoI7svQY8jNT/dIeyjy4hbO3xprQAjM36UHWwoWdQ2m6fkKl5Q5I0j53iruyRkrzDF-vG1SaBBRk-lrOX0w7lHFb8pAnAn-h2UbfebYrtswnuYt9/bzD==",
            ),
            (
                "aid=6383&web_rid=734977595361&is_need_double_stream=false",
                "{\"a\":1}",
                options(
                    1_730_000_000_123,
                    1_730_000_000_456,
                    [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
                    SIGN_UA,
                    "1920|1080|0|0|Win32",
                ),
                "DfmpkD62kjEsdEu856xLfY3q65D2YQic0SVkMD2fBYEPbL39HMTa9exoI7svQyujNT/dIcEjy4hbO3xprQAjM36UHmJx/DqAlLDpMj==",
            ),
            (
                "aid=6383&language=zh-CN",
                "",
                options(
                    1_762_300_000_000,
                    1_762_300_000_042,
                    [200, 100, 50, 25, 12, 6, 3, 1, 0, 255, 128, 64],
                    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
                    SIGN_FP,
                ),
                "n2fn2fj2DjgDebkD5UILfY3q6313YQRj0SVkMD2f4VvLD639HMYD9exoJiivn5LjiT/QIeyjy4hbO3xprQAjM36UHWwoWdQ2m6fkKl5Q5I0j53iruyRkrzDF-vG1SaBBRk-lrOX0w7lHFb8pAnAn-h2UbfebYrtswnuYt9/bCD==",
            ),
        ];

        for (params, body, opts, expected) in cases {
            assert_eq!(raw(params, body, &opts), expected, "params={params}");
        }
    }

    #[test]
    fn sign_appends_query() {
        let opts = options(
            1_730_000_000_000,
            1_730_000_000_000,
            [7, 11, 23, 101, 102, 103, 204, 205, 206, 1, 2, 3],
            SIGN_UA,
            SIGN_FP,
        );
        let signed = sign("aid=6383&live_id=1", "", &opts);
        assert!(signed.starts_with("aid=6383&live_id=1&a_bogus="));
        assert_eq!(signed.split("a_bogus=").nth(1).map(str::len), Some(172));
    }

    #[test]
    fn random_prefix_follows_browser_pattern() {
        // 前缀不是随手取的 12 个随机字节：服务端按位模式校验，全 0 前缀实测会拿到空响应。
        for _ in 0..64 {
            let prefix = random_prefix();
            for chunk in prefix.chunks(4) {
                assert_eq!(chunk[0] & 0x55, 0x01);
                assert_eq!(chunk[1] & 0xaa, 0x02);
                assert_eq!(chunk[2] & 0x55, 0x05);
                assert_eq!(chunk[3] & 0xaa, 0x28);
            }
        }
    }

    #[test]
    fn sm3_known_answer() {
        // "abc" 的 SM3 摘要，来自 GB/T 32905 示例
        let digest = sm3_digest(b"abc");
        let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(
            hex,
            "66c7f0f462eeedd9d1f2d46bdc10e4e24167c4875cf2f7a2297da02b8f4ba8e0"
        );
    }
}
