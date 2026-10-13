//! VietQR: the text a Vietnamese banking app reads off a QR code to fill in a
//! transfer (EMVCo's merchant-presented code as NAPAS 247 uses it), and the
//! code's modules.

use unicode_normalization::UnicodeNormalization;

/// NAPAS's application id, and its service "transfer to an account".
const NAPAS_AID: &str = "A000000727";
const TO_ACCOUNT: &str = "QRIBFTTA";
/// A NAPAS 247 transfer stays under 500 million đồng.
pub const MAX_AMOUNT: u64 = 499_999_999;
/// The smallest code `modules` draws: version 9 holds any transfer's text
/// (180 bytes), so the code keeps its size while a name is typed.
const MIN_VERSION: u8 = 9;

/// One field: its two-digit id, the two-digit length of `value`, `value`.
fn field(id: &str, value: &str) -> String {
    debug_assert!(value.len() <= 99, "{id}: {value}");
    format!("{id}{:02}{value}", value.len())
}

/// CRC-16/CCITT-FALSE, which closes the text.
fn crc16(text: &str) -> u16 {
    text.bytes().fold(0xFFFF, |crc, byte| {
        (0..8).fold(crc ^ (u16::from(byte) << 8), |crc, _| {
            if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            }
        })
    })
}

/// The text of a code that pays into `account` at the bank NAPAS knows as
/// `bank`: `amount` đồng (0 leaves the amount to the payer), with `note` as
/// the transfer's message (see `plain_note`; empty for none).
pub fn payload(bank: &str, account: &str, amount: u64, note: &str) -> String {
    let payee = field("00", NAPAS_AID)
        + &field("01", &(field("00", bank) + &field("01", account)))
        + &field("02", TO_ACCOUNT);
    // "12": a code made for one payment; "11": one to pay into at will.
    let mut text = field("00", "01") + &field("01", if amount > 0 { "12" } else { "11" });
    text += &field("38", &payee);
    text += &field("53", "704");
    if amount > 0 {
        text += &field("54", &amount.min(MAX_AMOUNT).to_string());
    }
    text += &field("58", "VN");
    if !note.is_empty() {
        text += &field("62", &field("08", note));
    }
    text += "6304";
    let crc = crc16(&text);
    text + &format!("{crc:04X}")
}

/// `text` as a transfer's message carries it: no Vietnamese marks, only
/// letters, digits and single spaces, `max` characters at most.
pub fn plain_note(text: &str, max: usize) -> String {
    let mut plain = String::new();
    for c in text.nfd() {
        let c = match c {
            'đ' => 'd',
            'Đ' => 'D',
            c if c.is_ascii_alphanumeric() => c,
            // The marks a letter was taken apart into.
            '\u{0300}'..='\u{036f}' => continue,
            _ => ' ',
        };
        if c == ' ' && (plain.is_empty() || plain.ends_with(' ')) {
            continue;
        }
        plain.push(c);
    }
    plain.truncate(max);
    plain.trim_end().to_string()
}

/// A QR code: `side` modules a side, row by row, `true` for a dark one.
pub struct Modules {
    pub side: usize,
    pub dark: Vec<bool>,
}

/// The QR code of `text`, or `None` for more than a code holds.
pub fn modules(text: &str) -> Option<Modules> {
    use qrcodegen::{QrCode, QrCodeEcc, QrSegment, Version};
    let code = QrCode::encode_segments_advanced(
        &QrSegment::make_segments(text),
        QrCodeEcc::Medium,
        Version::new(MIN_VERSION),
        Version::MAX,
        None,
        true,
    )
    .ok()?;
    let side = code.size() as usize;
    let dark = (0..side * side)
        .map(|i| code.get_module((i % side) as i32, (i / side) as i32))
        .collect();
    Some(Modules { side, dark })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the ACB app's own code for account 18278961 reads as (the image
    /// the app showed before it drew the code itself).
    const FROM_THE_BANK: &str =
        "00020101021138520010A000000727012201081827896100069704160208QRIBFTTA620053037045802VN63046B4F";

    /// The fields of `text`, each with its id.
    fn fields(text: &str) -> Vec<(&str, &str)> {
        let mut fields = Vec::new();
        let mut rest = text;
        while rest.len() >= 4 {
            let len: usize = rest[2..4].parse().unwrap();
            fields.push((&rest[..2], &rest[4..4 + len]));
            rest = &rest[4 + len..];
        }
        assert!(rest.is_empty(), "{text}");
        fields
    }

    fn sorted<'a>(mut fields: Vec<(&'a str, &'a str)>) -> Vec<(&'a str, &'a str)> {
        fields.sort();
        fields
    }

    #[test]
    fn the_text_closes_with_the_checksum_the_bank_writes() {
        let (body, crc) = FROM_THE_BANK.split_at(FROM_THE_BANK.len() - 4);
        assert_eq!(format!("{:04X}", crc16(body)), crc);
    }

    /// The value of field `id` among `fields`.
    fn of<'a>(fields: &[(&'a str, &'a str)], id: &str) -> Option<&'a str> {
        fields.iter().find(|(i, _)| *i == id).map(|(_, v)| *v)
    }

    #[test]
    fn a_code_with_no_amount_names_the_account_as_the_banks_own_does() {
        let ours = payload("970416", "18278961", 0, "");
        let (ours, bank) = (fields(&ours), fields(FROM_THE_BANK));
        for id in ["00", "01", "53", "58"] {
            assert_eq!(of(&ours, id), of(&bank, id), "field {id}");
        }
        // The payee: the same service and account at the same bank. The bank
        // writes the account before the bank's id; a reader takes either.
        let (ours, bank) = (
            fields(of(&ours, "38").unwrap()),
            fields(of(&bank, "38").unwrap()),
        );
        assert_eq!(of(&ours, "00"), of(&bank, "00"));
        assert_eq!(of(&ours, "02"), of(&bank, "02"));
        assert_eq!(
            sorted(fields(of(&ours, "01").unwrap())),
            sorted(fields(of(&bank, "01").unwrap()))
        );
        assert_eq!(
            sorted(fields(of(&ours, "01").unwrap())),
            vec![("00", "970416"), ("01", "18278961")]
        );
    }

    #[test]
    fn the_amount_and_the_message_go_into_the_text() {
        assert_eq!(
            payload("970416", "18278961", 50_000, "iAi ung ho Nguyen Van A"),
            "00020101021238520010A000000727012200069704160108182789610208QRIBFTTA\
             53037045405500005802VN62270823iAi ung ho Nguyen Van A63044F2A"
        );
        assert_eq!(
            payload("970416", "18278961", 0, "iAi ung ho"),
            "00020101021138520010A000000727012200069704160108182789610208QRIBFTTA\
             53037045802VN62140810iAi ung ho6304E6D9"
        );
        assert_eq!(
            payload("970416", "18278961", 0, ""),
            "00020101021138520010A000000727012200069704160108182789610208QRIBFTTA\
             53037045802VN6304CAE5"
        );
        // No more than one NAPAS transfer carries.
        let most = payload("970416", "18278961", u64::MAX, "");
        assert!(most.contains("5409499999999"), "{most}");
    }

    #[test]
    fn a_message_is_plain_letters_digits_and_single_spaces() {
        assert_eq!(plain_note("Nguyễn Văn Á", 30), "Nguyen Van A");
        assert_eq!(plain_note("Đỗ Thị Hường", 30), "Do Thi Huong");
        assert_eq!(
            plain_note("TRƯƠNG QUỐC ĐẠT đặng", 30),
            "TRUONG QUOC DAT dang"
        );
        // Marks typed apart from their letters.
        assert_eq!(plain_note("Tra\u{0302}\u{0300}n", 30), "Tran");
        assert_eq!(
            plain_note("  Cty  A&B - in ấn (Q.1) ", 30),
            "Cty A B in an Q 1"
        );
        assert_eq!(plain_note("Nguyễn Thị Minh Khai", 10), "Nguyen Thi");
        assert_eq!(plain_note("Lê Văn Tám", 3), "Le");
        assert_eq!(plain_note("…!?", 30), "");
    }

    #[test]
    fn the_code_keeps_its_size_from_the_shortest_text_to_the_longest() {
        let short = payload("970416", "18278961", 0, "");
        let long = payload(
            "970416",
            "18278961",
            MAX_AMOUNT,
            &format!("iAi ung ho {}", "N".repeat(30)),
        );
        for text in [&short, &long] {
            let code = modules(text).unwrap();
            assert_eq!(code.side, 17 + 4 * MIN_VERSION as usize, "{text}");
            assert_eq!(code.dark.len(), code.side * code.side);
            // The three corner marks a scanner finds the code by.
            let dark = |x: usize, y: usize| code.dark[y * code.side + x];
            for (x, y) in [(0, 0), (code.side - 7, 0), (0, code.side - 7)] {
                assert!(dark(x, y) && dark(x + 6, y + 6) && dark(x + 3, y + 3));
                assert!(!dark(x + 1, y + 1) && !dark(x + 5, y + 5));
            }
        }
        assert!(modules(&"x".repeat(4000)).is_none());
    }
}
