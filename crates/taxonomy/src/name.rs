//! What a payment-method name may contain, and what makes two names the
//! same name.
//!
//! # The threat this exists for
//!
//! A merchant-defined method's name is arbitrary text that every client
//! renders in a picker, beside eighty-odd rails this build ships. If that
//! text may be anything, the first thing somebody writes is `M-Pesa` —
//! with a trailing space, or a Cyrillic `М`, or a zero-width joiner in the
//! middle — and a buyer choosing it believes they are being paid over
//! Safaricom's rails.
//!
//! Two separate defences, because they answer two separate questions.
//!
//! [`check_name`] answers "can this be rendered at all". It is the rule
//! `openfiat_reviews::record::is_display_hazard` applies to comment text,
//! plus the characters that matter for a *label* and not for prose: a
//! newline is ordinary in a review and is a way to push text out of its row
//! here, and an invisible character is meaningless in both but is the whole
//! attack in a name that is compared for equality.
//!
//! [`skeleton`] answers "is this a look-alike of something we already
//! ship". It folds a name to the shape a human eye sees — case, accents,
//! script confusables, fullwidth forms, digit-for-letter swaps, and every
//! separator dropped — so `М‑Рesa`, `m pesa` and `MPESA` all reduce to
//! `mpesa`. [`crate::record::MerchantPaymentMethod::validate`] refuses a
//! definition whose skeleton is one the catalog already answers to.
//!
//! # What this deliberately does not attempt
//!
//! The fold table is targeted, not the whole of UTS-39. It covers the
//! Latin-1 and Latin Extended-A accents, the Cyrillic and Greek letters
//! that are drawn as Latin ones, fullwidth ASCII, and the digits people
//! substitute for letters; it does not pretend to catch every confusable
//! pair in Unicode, and a determined impostor can still land on
//! `M-Pesa Kenya — Official`, whose skeleton is genuinely different.
//!
//! That is why the scoping rule in [`crate::record`] is the load-bearing
//! defence and this is the second line: a merchant-defined method is
//! selectable only by the merchant who defined it, and the client contract
//! (`docs/payment-methods.md`) requires it to be shown as that merchant's
//! own. A name check that claimed to make arbitrary text safe would be the
//! more dangerous of the two.

use crate::error::TaxonomyError;

/// Longest accepted name, in characters.
///
/// A definition is gossiped to every node and stored by every node
/// forever, so the same reasoning as `openfiat_reviews::MAX_COMMENT_CHARS`
/// applies at the length a label actually needs. Sixty-four characters is
/// comfortably more than the longest rail this build ships
/// (`FPS (Faster Payment System)`, 27) and far short of a paragraph.
pub const MAX_NAME_CHARS: usize = 64;

/// Whether a name is one a client can render and a person can read.
///
/// # Errors
///
/// [`TaxonomyError::MalformedDefinition`] for anything below. Refused
/// rather than normalised, deliberately: the bytes a merchant signed are
/// the bytes every node stores and every client draws, so silently
/// trimming a name here would mean the record on file is not the record
/// that was checked, and `M-Pesa ` and `M-Pesa` would be two entries that
/// print identically.
///
/// - empty, or longer than [`MAX_NAME_CHARS`];
/// - nothing but punctuation — a name has to reduce to *some* letter or
///   digit, or there is nothing to compare and nothing to read;
/// - leading, trailing or repeated spaces;
/// - any whitespace other than U+0020 — a no-break space and an ideographic
///   space are indistinguishable from an ordinary one on screen and are not
///   equal to it in memory;
/// - control characters and bidirectional overrides/isolates, which redraw
///   the text around them (`openfiat_reviews` refuses these in comments for
///   the same reason);
/// - characters that render as nothing at all: zero-width spaces and
///   joiners, the soft hyphen, word joiners, the byte-order mark, the
///   Unicode tag block. Each one exists purely to make two different
///   strings look like one string.
pub fn check_name(name: &str) -> Result<(), TaxonomyError> {
    let length = name.chars().count();
    if length == 0 || length > MAX_NAME_CHARS {
        return Err(TaxonomyError::MalformedDefinition);
    }
    if name.starts_with(' ') || name.ends_with(' ') || name.contains("  ") {
        return Err(TaxonomyError::MalformedDefinition);
    }
    for c in name.chars() {
        let acceptable = !c.is_control()
            && !is_invisible(c)
            && (!c.is_whitespace() || c == ' ')
            && !matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}');
        if !acceptable {
            return Err(TaxonomyError::MalformedDefinition);
        }
    }
    if skeleton(name).is_empty() {
        return Err(TaxonomyError::MalformedDefinition);
    }
    Ok(())
}

/// Characters that occupy no space on screen.
///
/// Whitespace is not here — a space is visible by being a gap, and
/// [`check_name`] handles it separately. These are the ones that render as
/// nothing whatsoever, so `M-Pe\u{200B}sa` and `M-Pesa` are the same
/// picture and different strings.
fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2060}'..='\u{2064}'
            | '\u{206A}'..='\u{206F}'
            | '\u{2800}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{E0000}'..='\u{E007F}'
    )
}

/// A name reduced to what a reader actually sees.
///
/// Lowercased, accents removed, script confusables and fullwidth forms
/// mapped to their Latin look-alikes, digits mapped to the letters they
/// stand in for, and everything that is not a letter or a digit dropped.
/// Two names with the same skeleton are two spellings of one label.
///
/// Non-Latin scripts survive intact rather than being dropped: a rail
/// named `支付宝` has a skeleton of `支付宝`, not of nothing. Reducing
/// every CJK name to the empty string would make them all equal to each
/// other, which is the opposite of what this is for — and is why
/// [`check_name`] refuses a name whose skeleton *is* empty.
pub fn skeleton(name: &str) -> String {
    let mut folded = String::with_capacity(name.len());
    for original in name.chars() {
        // Fullwidth ASCII is the same alphabet at a fixed offset, and is
        // the cheapest way to write a name that is byte-different and
        // pixel-similar.
        let wide_folded = match original {
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(original as u32 - 0xFEE0).unwrap_or(original),
            _ => original,
        };
        for lowered in wide_folded.to_lowercase() {
            match fold(lowered) {
                Some(latin) => folded.push_str(latin),
                None if lowered.is_alphanumeric() => folded.push(lowered),
                None => {}
            }
        }
    }
    folded
}

/// One lowercased character's representative, if it belongs to a set a
/// reader would not tell apart.
///
/// Note that this maps *classes*, not "wrong spellings to right ones".
/// `i`, `l`, `1` and their accented and Cyrillic cousins all fold to `l`,
/// which is not because any of them is really an `l` — it is because a
/// person skimming a picker cannot tell `PIX` from `P1X` from `PlX`, and a
/// check that folded only two of the three would be a check an attacker
/// walks around by choosing the third.
///
/// `None` means "no class" — the caller keeps the character when it is
/// alphanumeric and drops it otherwise.
fn fold(c: char) -> Option<&'static str> {
    Some(match c {
        // Digits standing in for letters, the oldest trick there is.
        '0' => "o",
        '1' | 'i' => "l",
        '3' => "e",
        '4' => "a",
        '5' => "s",
        '7' => "t",
        // Latin-1 Supplement and Latin Extended-A: the accent is not a
        // different letter to somebody skimming a list.
        'à'..='å' | 'ā' | 'ă' | 'ą' => "a",
        'æ' => "ae",
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => "c",
        'ð' | 'ď' | 'đ' => "d",
        'è'..='ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => "e",
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => "g",
        'ĥ' | 'ħ' => "h",
        'ì'..='ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => "l",
        'ĵ' => "j",
        'ķ' => "k",
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => "l",
        'ñ' | 'ń' | 'ņ' | 'ň' => "n",
        'ò'..='ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => "o",
        'œ' => "oe",
        'ŕ' | 'ŗ' | 'ř' => "r",
        'ś' | 'ŝ' | 'ş' | 'š' => "s",
        'ß' => "ss",
        'ţ' | 'ť' | 'ŧ' => "t",
        'þ' => "th",
        'ù'..='ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => "u",
        'ŵ' => "w",
        'ý' | 'ÿ' | 'ŷ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        // Cyrillic letters drawn as Latin ones. This is the whole of the
        // homoglyph attack in practice: `М-Реѕа` is four substitutions.
        'а' => "a",
        'в' => "b",
        'с' => "c",
        'ԁ' => "d",
        'е' | 'ё' => "e",
        'һ' | 'н' => "h",
        'і' => "l",
        'ј' => "j",
        'к' => "k",
        'ӏ' => "l",
        'м' => "m",
        'о' => "o",
        'р' => "p",
        'ԛ' => "q",
        'ѕ' => "s",
        'т' => "t",
        'у' => "y",
        'ԝ' => "w",
        'х' => "x",
        // Greek, for the same reason and with the same eye.
        'α' => "a",
        'β' => "b",
        'ε' => "e",
        'η' => "n",
        'ι' => "l",
        'κ' => "k",
        'ν' => "v",
        'ο' => "o",
        'ρ' => "p",
        'τ' => "t",
        'υ' => "u",
        'χ' => "x",
        'ζ' => "z",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_name_is_accepted() {
        for name in [
            "Acme Pay",
            "Banco de Crédito",
            "支付宝",
            "M-Pesa Kenya — Merchant Till 4421",
        ] {
            assert_eq!(check_name(name), Ok(()), "{name:?}");
        }
    }

    /// The trailing-space attack, refused rather than trimmed. Trimming
    /// would store bytes nobody signed; accepting would put two entries in
    /// the picker that print the same.
    #[test]
    fn stray_whitespace_is_refused_rather_than_normalised() {
        for name in ["M-Pesa ", " M-Pesa", "M-Pesa  Kenya", "M-Pesa\u{00A0}Kenya"] {
            assert_eq!(
                check_name(name),
                Err(TaxonomyError::MalformedDefinition),
                "{name:?}"
            );
        }
    }

    #[test]
    fn a_name_that_could_redraw_the_row_around_it_is_refused() {
        for name in [
            "\u{202E}yaP emcA",
            "Acme\nPay",
            "Acme\rPay",
            "Acme\u{0}Pay",
            "Acme\u{2066}Pay",
        ] {
            assert_eq!(
                check_name(name),
                Err(TaxonomyError::MalformedDefinition),
                "{name:?} must not reach a renderer"
            );
        }
    }

    /// Each of these prints as `M-Pesa` and is a different string, which
    /// is the entire point of using them.
    #[test]
    fn characters_that_render_as_nothing_are_refused() {
        for name in [
            "M-Pe\u{200B}sa",
            "M-Pesa\u{200D}",
            "M\u{00AD}-Pesa",
            "M-Pesa\u{FEFF}",
            "M-Pesa\u{E0041}",
        ] {
            assert_eq!(
                check_name(name),
                Err(TaxonomyError::MalformedDefinition),
                "{name:?}"
            );
        }
    }

    #[test]
    fn a_name_must_be_bounded_and_must_say_something() {
        assert_eq!(
            check_name(&"a".repeat(MAX_NAME_CHARS + 1)),
            Err(TaxonomyError::MalformedDefinition)
        );
        assert_eq!(check_name(&"a".repeat(MAX_NAME_CHARS)), Ok(()));
        assert_eq!(check_name(""), Err(TaxonomyError::MalformedDefinition));
        assert_eq!(
            check_name("--- ***"),
            Err(TaxonomyError::MalformedDefinition),
            "a name with no letter or digit in it is not a name"
        );
    }

    /// The skeleton is the impersonation check, so this is the test that
    /// says what it is worth: every one of these is a way of writing
    /// `M-Pesa` that a picker would render indistinguishably.
    #[test]
    fn every_way_of_writing_m_pesa_reduces_to_the_same_skeleton() {
        for spelling in [
            "M-Pesa",
            "m pesa",
            "MPESA",
            "M.Pesa",
            "М-Реѕа",       // Cyrillic М, Р, е, ѕ, а
            "Ｍ－Ｐｅｓａ", // fullwidth
            "M-Pes4",
            "M-Pésa",
        ] {
            assert_eq!(skeleton(spelling), "mpesa", "{spelling:?}");
        }
    }

    #[test]
    fn a_skeleton_keeps_scripts_it_cannot_fold_rather_than_erasing_them() {
        assert_eq!(skeleton("支付宝"), "支付宝");
        assert_ne!(
            skeleton("支付宝"),
            skeleton("微信支付"),
            "two CJK names must not collapse into one another"
        );
    }

    #[test]
    fn different_rails_keep_different_skeletons() {
        assert_ne!(skeleton("Alipay"), skeleton("AlipayHK"));
        assert_ne!(skeleton("WeChat Pay"), skeleton("WeChat Pay HK"));
        assert_ne!(skeleton("Tigo Pesa"), skeleton("Tigo Money"));
    }
}
