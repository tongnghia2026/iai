//! Text the skill library compares: Vietnamese folded to plain letters, the
//! words of a request, and names a folder may take.

/// `s` lower-cased with Vietnamese marks taken off: "Ảnh thẻ" → "anh the".
/// Combining marks (text typed in decomposed form) are dropped too.
pub fn fold(s: &str) -> String {
    s.chars()
        .flat_map(char::to_lowercase)
        .filter(|c| !('\u{300}'..='\u{36f}').contains(c))
        .map(base_letter)
        .collect()
}

fn base_letter(c: char) -> char {
    match c {
        'à' | 'á' | 'ạ' | 'ả' | 'ã' | 'â' | 'ầ' | 'ấ' | 'ậ' | 'ẩ' | 'ẫ' | 'ă' | 'ằ' | 'ắ' | 'ặ'
        | 'ẳ' | 'ẵ' => 'a',
        'è' | 'é' | 'ẹ' | 'ẻ' | 'ẽ' | 'ê' | 'ề' | 'ế' | 'ệ' | 'ể' | 'ễ' => 'e',
        'ì' | 'í' | 'ị' | 'ỉ' | 'ĩ' => 'i',
        'ò' | 'ó' | 'ọ' | 'ỏ' | 'õ' | 'ô' | 'ồ' | 'ố' | 'ộ' | 'ổ' | 'ỗ' | 'ơ' | 'ờ' | 'ớ' | 'ợ'
        | 'ở' | 'ỡ' => 'o',
        'ù' | 'ú' | 'ụ' | 'ủ' | 'ũ' | 'ư' | 'ừ' | 'ứ' | 'ự' | 'ử' | 'ữ' => 'u',
        'ỳ' | 'ý' | 'ỵ' | 'ỷ' | 'ỹ' => 'y',
        'đ' => 'd',
        other => other,
    }
}

/// Words that say nothing about which job is meant, folded. "the" is not
/// one: folded, it is "thẻ".
const STOP: &[&str] = &[
    "cho", "cac", "cua", "va", "la", "mot", "nhung", "thi", "ra", "vao", "o", "trong", "voi",
    "nay", "do", "len", "de", "duoc", "hay", "giup", "toi", "minh", "ban", "lam", "tat", "ca",
    "het", "moi", "nhe", "nhu", "sau", "khi", "tu", "den", "bang", "co", "a", "an", "and", "of",
    "to", "in", "on", "for", "with", "from", "into", "all", "each", "please", "make", "me",
];

/// The words of `s` that tell jobs apart, folded, each once, in order.
pub fn words(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in fold(s).split(|c: char| !c.is_alphanumeric()) {
        if w.is_empty()
            || STOP.contains(&w)
            || (w.chars().count() < 2 && !w.chars().all(|c| c.is_ascii_digit()))
        {
            continue;
        }
        if !out.iter().any(|o| o == w) {
            out.push(w.to_string());
        }
    }
    out
}

/// A folder name for `s`: folded, `a-z0-9` and `-`, at most 48 characters.
pub fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in fold(s).chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
        if out.len() > 48 {
            // Cut at the last whole word.
            let cut = out.get(..48).and_then(|head| head.rfind('-')).unwrap_or(48);
            out.truncate(cut);
            break;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "ky-nang".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vietnamese_folds_to_plain_letters_either_way_it_was_typed() {
        assert_eq!(fold("Ảnh THẺ nền Xanh"), "anh the nen xanh");
        assert_eq!(fold("Đường dẫn ữ ỹ"), "duong dan u y");
        // "ả" typed as "a" + combining hook above.
        assert_eq!(fold("A\u{309}nh"), "anh");
    }

    #[test]
    fn a_request_reads_as_the_words_that_tell_it_apart() {
        assert_eq!(
            words("Làm ảnh thẻ 3x4 nền xanh cho tất cả ảnh trong thư mục D:\\Khach"),
            ["anh", "the", "3x4", "nen", "xanh", "thu", "muc", "khach"]
        );
        assert_eq!(words("thiệp 20/10"), ["thiep", "20", "10"]);
    }

    #[test]
    fn a_name_becomes_a_safe_folder_name() {
        assert_eq!(slug("Ảnh thẻ hàng loạt → tờ in"), "anh-the-hang-loat-to-in");
        assert_eq!(slug("../../Windows"), "windows");
        assert_eq!(slug("***"), "ky-nang");
        assert!(slug(&"dài ".repeat(40)).len() <= 48);
    }
}
