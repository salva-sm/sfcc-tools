//! `x.custom.y` and preference reads, with the type of `x` guessed from its name.
//! Conservative: an unknown receiver yields nothing, an ambiguous one yields every candidate type.

use crate::metadata::SITE_PREFERENCES;

pub const PREFERENCE_CALL: &str = "getCustomPreferenceValue";

const NOT_ATTRIBUTES: [&str; 7] = [
    "hasOwnProperty",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toString",
    "toLocaleString",
    "valueOf",
    "constructor",
];

/// Variable-name suffixes, longest first, and the types they may stand for.
/// Several means the name does not decide: `paymentInstrument` is an order's or a customer's.
const SUBJECTS: &[(&str, &[&str])] = &[
    ("bonusdiscountlineitem", &["BonusDiscountLineItem"]),
    ("giftcertificatelineitem", &["GiftCertificateLineItem"]),
    ("slotconfiguration", &["SlotConfiguration"]),
    ("shippinglineitem", &["ShippingLineItem"]),
    ("productlineitem", &["ProductLineItem"]),
    ("couponlineitem", &["CouponLineItem"]),
    (
        "paymentinstrument",
        &["OrderPaymentInstrument", "CustomerPaymentInstrument"],
    ),
    ("paymenttransaction", &["PaymentTransaction"]),
    // Only a qualified address name is mapped: a custom object is very often
    // called `..._address`, and its attributes are its own.
    ("customeraddress", &["CustomerAddress"]),
    ("shippingaddress", &["OrderAddress"]),
    ("billingaddress", &["OrderAddress"]),
    ("giftcertificate", &["GiftCertificate"]),
    ("sourcecodegroup", &["SourceCodeGroup"]),
    ("variationgroup", &["Product"]),
    ("customergroup", &["CustomerGroup"]),
    ("shippingmethod", &["ShippingMethod"]),
    ("shippingorder", &["ShippingOrder"]),
    ("slotcontent", &["SlotConfiguration"]),
    ("appeasement", &["Appeasement"]),
    ("lineitem", &["ProductLineItem"]),
    ("promotion", &["Promotion"]),
    ("campaign", &["Campaign"]),
    ("shipment", &["Shipment"]),
    ("customer", &["Customer"]),
    ("category", &["Category"]),
    ("product", &["Product"]),
    ("content", &["Content"]),
    ("invoice", &["Invoice"]),
    ("profile", &["Profile"]),
    ("session", &["Session"]),
    ("basket", &["Basket"]),
    ("coupon", &["Coupon"]),
    ("folder", &["Folder"]),
    ("return", &["Return"]),
    ("order", &["Order"]),
    ("store", &["Store"]),
    ("variant", &["Product"]),
    ("master", &["Product"]),
    ("cart", &["Basket"]),
];

#[derive(Debug, PartialEq, Eq)]
pub struct Access {
    pub types: &'static [&'static str],
    pub attribute: String,
    /// Character offset of the attribute name.
    pub start: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Pending {
    pub types: &'static [&'static str],
    /// Characters of the attribute name already typed.
    pub typed: usize,
}

pub fn types_of(receiver: &str) -> Option<&'static [&'static str]> {
    let lowered = receiver.to_ascii_lowercase();
    SUBJECTS
        .iter()
        .find(|(suffix, _)| lowered.ends_with(suffix))
        .map(|(_, types)| *types)
}

pub fn accesses(line: &str) -> Vec<Access> {
    let chars: Vec<char> = line.chars().collect();
    let mut found = Vec::new();
    let mut index = 0;
    while let Some(marker) = find_from(&chars, ".custom.", index) {
        let start = marker + ".custom.".len();
        let end = identifier_end(&chars, start);
        index = start.max(marker + 1);
        if end == start {
            continue;
        }
        let attribute: String = chars[start..end].iter().collect();
        index = end;
        let Some(types) = types_of(&identifier_before(&chars, marker)) else {
            continue;
        };
        if is_not_an_attribute(&attribute) {
            continue;
        }
        found.push(Access {
            types,
            attribute,
            start,
        });
    }
    found
}

pub fn pending(head: &str) -> Option<Pending> {
    let chars: Vec<char> = head.chars().collect();
    let typed = trailing_identifier_len(&chars);
    let marker = chars.len() - typed;
    let prefix: String = chars[..marker].iter().collect();
    let receiver_end = marker.checked_sub(".custom.".len())?;
    if !prefix.ends_with(".custom.") {
        return None;
    }
    let types = types_of(&identifier_before(&chars, receiver_end))?;
    Some(Pending { types, typed })
}

pub fn preferences(line: &str) -> Vec<Access> {
    let chars: Vec<char> = line.chars().collect();
    let mut found = Vec::new();
    let mut index = 0;
    while let Some(call) = find_from(&chars, PREFERENCE_CALL, index) {
        index = call + PREFERENCE_CALL.len();
        let Some(literal) = whole_argument(&chars, index) else {
            continue;
        };
        index = literal.start;
        if is_not_an_attribute(&literal.text) {
            continue;
        }
        found.push(Access {
            types: SITE_PREFERENCE_TYPES,
            attribute: literal.text,
            start: literal.start,
        });
    }
    found
}

const SITE_PREFERENCE_TYPES: &[&str] = &[SITE_PREFERENCES];

pub fn is_pending_preference(head: &str) -> bool {
    let Some(quote) = head.rfind(['\'', '"']) else {
        return false;
    };
    let before = head[..quote].trim_end();
    before
        .strip_suffix('(')
        .is_some_and(|call| call.trim_end().ends_with(PREFERENCE_CALL))
}

/// Present at runtime without metadata: plain JS object members and platform `__` injections.
fn is_not_an_attribute(name: &str) -> bool {
    name.starts_with("__") || NOT_ATTRIBUTES.contains(&name)
}

struct Literal {
    text: String,
    start: usize,
}

/// The quoted literal that is the *whole* argument at `from`; a concatenated one names no single preference.
fn whole_argument(chars: &[char], from: usize) -> Option<Literal> {
    let mut index = from;
    while index < chars.len() && (chars[index].is_whitespace() || chars[index] == '(') {
        index += 1;
    }
    let quote = *chars.get(index)?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    let start = index + 1;
    let mut end = start;
    while end < chars.len() && chars[end] != quote {
        end += 1;
    }
    if end >= chars.len() {
        return None;
    }
    let mut after = end + 1;
    while after < chars.len() && chars[after].is_whitespace() {
        after += 1;
    }
    if chars.get(after).is_some_and(|c| *c == '+') {
        return None;
    }
    Some(Literal {
        text: chars[start..end].iter().collect(),
        start,
    })
}

fn find_from(chars: &[char], needle: &str, from: usize) -> Option<usize> {
    let needle: Vec<char> = needle.chars().collect();
    if needle.is_empty() || chars.len() < needle.len() {
        return None;
    }
    (from..=chars.len() - needle.len())
        .find(|index| chars[*index..index + needle.len()] == needle[..])
}

fn is_identifier(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

fn identifier_end(chars: &[char], from: usize) -> usize {
    let mut end = from;
    while end < chars.len() && is_identifier(chars[end]) {
        end += 1;
    }
    end
}

/// The identifier ending at `end`, ignoring a trailing `()` call.
fn identifier_before(chars: &[char], end: usize) -> String {
    let mut cursor = end;
    if cursor >= 2 && chars[cursor - 1] == ')' && chars[cursor - 2] == '(' {
        cursor -= 2;
    }
    let mut start = cursor;
    while start > 0 && is_identifier(chars[start - 1]) {
        start -= 1;
    }
    chars[start..cursor].iter().collect()
}

fn trailing_identifier_len(chars: &[char]) -> usize {
    let mut length = 0;
    while length < chars.len() && is_identifier(chars[chars.len() - 1 - length]) {
        length += 1;
    }
    length
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_a_variable_name_to_an_object_type() {
        assert_eq!(types_of("product"), Some(&["Product"][..]));
        assert_eq!(types_of("apiProduct"), Some(&["Product"][..]));
        assert_eq!(types_of("currentBasket"), Some(&["Basket"][..]));
        assert_eq!(types_of("productLineItem"), Some(&["ProductLineItem"][..]));
        assert_eq!(types_of("result"), None);
    }

    #[test]
    fn keeps_both_types_when_the_name_does_not_decide() {
        assert_eq!(
            types_of("paymentInstrument"),
            Some(&["OrderPaymentInstrument", "CustomerPaymentInstrument"][..])
        );
    }

    #[test]
    fn reads_slot_content_as_a_slot_configuration() {
        assert_eq!(types_of("slotcontent"), Some(&["SlotConfiguration"][..]));
    }

    #[test]
    fn finds_an_access_and_where_the_attribute_starts() {
        let line = "if (apiProduct.custom.season) {";
        let found = accesses(line);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].types, ["Product"]);
        assert_eq!(found[0].attribute, "season");
        assert_eq!(found[0].start, line.find("season").unwrap());
    }

    #[test]
    fn ignores_a_receiver_it_cannot_place() {
        assert!(accesses("thing.custom.whatever").is_empty());
    }

    #[test]
    fn ignores_the_members_every_javascript_object_has() {
        assert!(accesses("product.custom.hasOwnProperty('x')").is_empty());
        assert!(accesses("slotcontent.custom.__SlotName").is_empty());
    }

    #[test]
    fn finds_several_accesses_in_one_line() {
        let found = accesses("order.custom.a + basket.custom.b");
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].types, ["Order"]);
        assert_eq!(found[1].types, ["Basket"]);
    }

    #[test]
    fn recognises_an_access_being_typed() {
        assert_eq!(
            pending("    if (product.custom.gue"),
            Some(Pending {
                types: &["Product"],
                typed: 3
            })
        );
        assert_eq!(
            pending("    if (order.custom."),
            Some(Pending {
                types: &["Order"],
                typed: 0
            })
        );
        assert_eq!(pending("    if (product.cus"), None);
    }

    #[test]
    fn reads_a_site_preference_through_its_getter() {
        let found = preferences("Site.getCurrent().getCustomPreferenceValue('newsletterEnabled')");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].types, [SITE_PREFERENCES]);
        assert_eq!(found[0].attribute, "newsletterEnabled");
    }

    #[test]
    fn ignores_a_preference_name_that_is_built_by_concatenation() {
        assert!(preferences("getCustomPreferenceValue('logo_' + locale)").is_empty());
        assert!(preferences("getCustomPreferenceValue(prefix + 'logo')").is_empty());
    }

    #[test]
    fn recognises_a_preference_being_typed() {
        assert!(is_pending_preference(
            "Site.getCurrent().getCustomPreferenceValue('loyal"
        ));
        assert!(!is_pending_preference("Resource.msg('label"));
    }
}
