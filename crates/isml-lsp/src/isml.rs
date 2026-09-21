//! The ISML tag set, as the platform defines it.
//!
//! Without this an editor falls back to HTML, which offers `<is:include>` and
//! other tags that do not exist. Serving the real set from the language server
//! means the wrong one stops being offered.

/// One ISML tag, with everything needed to offer and insert it.
pub struct Tag {
    /// The tag name, without the angle bracket.
    pub name: &'static str,
    /// One line on what the tag does.
    pub summary: &'static str,
    /// What goes between it and its closing tag.
    pub body: Body,
    /// The attributes the platform accepts on it.
    pub attributes: &'static [Attribute],
}

/// What goes between the tag and its closing tag, which decides the snippet.
pub enum Body {
    /// `<isinclude ... />`
    Empty,
    /// `<isif ...> ... </isif>`
    Inline,
    /// `<isscript> ... </isscript>`, opened on its own line.
    Block,
}

/// One attribute of a tag.
pub struct Attribute {
    /// The attribute name.
    pub name: &'static str,
    /// One line on what it controls.
    pub summary: &'static str,
    /// Whether the platform refuses the tag without it.
    pub required: bool,
    /// The values the platform accepts, when it is a closed set.
    pub values: &'static [&'static str],
}

const NONE: &[&str] = &[];
const SCOPES: &[&str] = &["page", "request", "session"];
const ENCODINGS: &[&str] = &["on", "off", "html", "xml", "wml", "jshtml", "jsonvalue"];

/// One tag by name.
pub fn tag(name: &str) -> Option<&'static Tag> {
    TAGS.iter().find(|candidate| candidate.name == name)
}

/// Every tag the platform defines.
pub fn tags() -> &'static [Tag] {
    TAGS
}

/// Tags whose attributes name a template, so their value completes to one.
pub const TEMPLATE_ATTRIBUTE: &str = "template";

/// The tag set itself.
pub static TAGS: &[Tag] = &[
    Tag {
        name: "isif",
        summary: "Renders its body when the condition holds.",
        body: Body::Inline,
        attributes: &[Attribute {
            name: "condition",
            summary: "Expression to evaluate.",
            required: true,
            values: NONE,
        }],
    },
    Tag {
        name: "iselseif",
        summary: "Further branch of an <isif>.",
        body: Body::Empty,
        attributes: &[Attribute {
            name: "condition",
            summary: "Expression to evaluate.",
            required: true,
            values: NONE,
        }],
    },
    Tag {
        name: "iselse",
        summary: "Fallback branch of an <isif>.",
        body: Body::Empty,
        attributes: NONE_ATTRIBUTES,
    },
    Tag {
        name: "isloop",
        summary: "Iterates over a collection or iterator.",
        body: Body::Inline,
        attributes: &[
            Attribute {
                name: "items",
                summary: "Collection to iterate over.",
                required: true,
                values: NONE,
            },
            Attribute {
                name: "var",
                summary: "Variable bound to the current element.",
                required: true,
                values: NONE,
            },
            Attribute {
                name: "status",
                summary: "Variable holding count, index, first, last, odd, even.",
                required: false,
                values: NONE,
            },
            Attribute {
                name: "begin",
                summary: "Index to start at.",
                required: false,
                values: NONE,
            },
            Attribute {
                name: "end",
                summary: "Index to stop at.",
                required: false,
                values: NONE,
            },
            Attribute {
                name: "step",
                summary: "Increment between iterations.",
                required: false,
                values: NONE,
            },
            Attribute {
                name: "iterator",
                summary: "Deprecated alias of items.",
                required: false,
                values: NONE,
            },
        ],
    },
    Tag {
        name: "isbreak",
        summary: "Leaves the innermost loop.",
        body: Body::Empty,
        attributes: NONE_ATTRIBUTES,
    },
    Tag {
        name: "iscontinue",
        summary: "Skips to the next iteration.",
        body: Body::Empty,
        attributes: NONE_ATTRIBUTES,
    },
    Tag {
        name: "isnext",
        summary: "Advances the iterator by one position.",
        body: Body::Empty,
        attributes: NONE_ATTRIBUTES,
    },
    Tag {
        name: "isinclude",
        summary: "Includes another template, or the output of a URL.",
        body: Body::Empty,
        attributes: &[
            Attribute {
                name: "template",
                summary: "Template path, without the .isml suffix.",
                required: false,
                values: NONE,
            },
            Attribute {
                name: "url",
                summary: "Remote include: a URL, usually from URLUtils.",
                required: false,
                values: NONE,
            },
            Attribute {
                name: "sf-toolkit",
                summary: "Storefront Toolkit decoration.",
                required: false,
                values: &["on", "off"],
            },
        ],
    },
    Tag {
        name: "isdecorate",
        summary: "Wraps the body in a decorator template.",
        body: Body::Inline,
        attributes: &[Attribute {
            name: "template",
            summary: "Decorator template path.",
            required: true,
            values: NONE,
        }],
    },
    Tag {
        name: "isreplace",
        summary: "Marks where a decorator inserts the decorated content.",
        body: Body::Empty,
        attributes: &[Attribute {
            name: "name",
            summary: "Named replacement region.",
            required: false,
            values: NONE,
        }],
    },
    Tag {
        name: "ismodule",
        summary: "Declares a custom tag backed by a template.",
        body: Body::Empty,
        attributes: &[
            Attribute {
                name: "template",
                summary: "Template implementing the tag.",
                required: true,
                values: NONE,
            },
            Attribute {
                name: "name",
                summary: "Name of the custom tag.",
                required: true,
                values: NONE,
            },
            Attribute {
                name: "attribute",
                summary: "Attribute the custom tag accepts. Repeatable.",
                required: false,
                values: NONE,
            },
        ],
    },
    Tag {
        name: "iscomponent",
        summary: "Remote include of a controller or pipeline.",
        body: Body::Empty,
        attributes: &[
            Attribute {
                name: "pipeline",
                summary: "Controller-Action or pipeline to call.",
                required: true,
                values: NONE,
            },
            Attribute {
                name: "locale",
                summary: "Locale to render the component in.",
                required: false,
                values: NONE,
            },
        ],
    },
    Tag {
        name: "isslot",
        summary: "Placeholder filled from Business Manager.",
        body: Body::Empty,
        attributes: &[
            Attribute {
                name: "id",
                summary: "Slot id.",
                required: true,
                values: NONE,
            },
            Attribute {
                name: "description",
                summary: "What the slot is for, shown in Business Manager.",
                required: true,
                values: NONE,
            },
            Attribute {
                name: "context",
                summary: "Configuration context of the slot.",
                required: true,
                values: &["global", "category", "folder"],
            },
            Attribute {
                name: "context-object",
                summary: "Category or folder the context refers to.",
                required: false,
                values: NONE,
            },
            Attribute {
                name: "preview-url",
                summary: "URL used to preview the slot.",
                required: false,
                values: NONE,
            },
        ],
    },
    Tag {
        name: "isprint",
        summary: "Formats and encodes a value for output.",
        body: Body::Empty,
        attributes: &[
            Attribute {
                name: "value",
                summary: "Expression to print.",
                required: true,
                values: NONE,
            },
            Attribute {
                name: "encoding",
                summary: "Output encoding. Only turn it off for trusted content.",
                required: false,
                values: ENCODINGS,
            },
            Attribute {
                name: "style",
                summary: "Named format for money, numbers and dates.",
                required: false,
                values: &[
                    "MONEY_SHORT",
                    "MONEY_LONG",
                    "INTEGER",
                    "DECIMAL",
                    "QUANTITY_SHORT",
                    "QUANTITY_LONG",
                    "DATE_SHORT",
                    "DATE_LONG",
                    "DATE_TIME",
                ],
            },
            Attribute {
                name: "formatter",
                summary: "Explicit format pattern.",
                required: false,
                values: NONE,
            },
            Attribute {
                name: "timezone",
                summary: "Time zone a date is rendered in.",
                required: false,
                values: &["SITE", "INSTANCE", "utc"],
            },
            Attribute {
                name: "padding",
                summary: "Minimum width, padded with spaces.",
                required: false,
                values: NONE,
            },
        ],
    },
    Tag {
        name: "isset",
        summary: "Defines a variable in a scope.",
        body: Body::Empty,
        attributes: &[
            Attribute {
                name: "name",
                summary: "Variable name.",
                required: true,
                values: NONE,
            },
            Attribute {
                name: "value",
                summary: "Value to assign.",
                required: true,
                values: NONE,
            },
            Attribute {
                name: "scope",
                summary: "Where the variable lives.",
                required: false,
                values: SCOPES,
            },
        ],
    },
    Tag {
        name: "isremove",
        summary: "Removes a variable from a scope.",
        body: Body::Empty,
        attributes: &[
            Attribute {
                name: "name",
                summary: "Variable name.",
                required: true,
                values: NONE,
            },
            Attribute {
                name: "scope",
                summary: "Where the variable lives.",
                required: false,
                values: SCOPES,
            },
        ],
    },
    Tag {
        name: "isscript",
        summary: "Server-side JavaScript with the dw.* API available.",
        body: Body::Block,
        attributes: NONE_ATTRIBUTES,
    },
    Tag {
        name: "iscomment",
        summary: "Comment stripped before the response is sent.",
        body: Body::Block,
        attributes: NONE_ATTRIBUTES,
    },
    Tag {
        name: "iscache",
        summary: "Caching policy for the page.",
        body: Body::Empty,
        attributes: &[
            Attribute {
                name: "type",
                summary: "Expiry strategy.",
                required: true,
                values: &["relative", "daily"],
            },
            Attribute {
                name: "hour",
                summary: "Hours to cache for, or hour of the day when daily.",
                required: false,
                values: NONE,
            },
            Attribute {
                name: "minute",
                summary: "Minutes to cache for.",
                required: false,
                values: NONE,
            },
            Attribute {
                name: "varyby",
                summary: "Adds price and promotion to the cache key.",
                required: false,
                values: &["price_promotion"],
            },
            Attribute {
                name: "status",
                summary: "Turns caching off for this template.",
                required: false,
                values: &["on", "off"],
            },
        ],
    },
    Tag {
        name: "iscontent",
        summary: "Sets the content type and encoding of the response.",
        body: Body::Empty,
        attributes: &[
            Attribute {
                name: "type",
                summary: "MIME type.",
                required: false,
                values: &["text/html", "text/xml", "application/json", "text/plain"],
            },
            Attribute {
                name: "encoding",
                summary: "Output encoding for the whole template.",
                required: false,
                values: ENCODINGS,
            },
            Attribute {
                name: "charset",
                summary: "Character set.",
                required: false,
                values: &["UTF-8", "ISO-8859-1"],
            },
            Attribute {
                name: "compact",
                summary: "Collapses whitespace in the output.",
                required: false,
                values: &["true", "false"],
            },
        ],
    },
    Tag {
        name: "isredirect",
        summary: "Sends an HTTP redirect.",
        body: Body::Empty,
        attributes: &[
            Attribute {
                name: "location",
                summary: "Target URL.",
                required: true,
                values: NONE,
            },
            Attribute {
                name: "permanent",
                summary: "301 instead of 302.",
                required: false,
                values: &["true", "false"],
            },
        ],
    },
    Tag {
        name: "isstatus",
        summary: "Sets the HTTP status code of the response.",
        body: Body::Empty,
        attributes: &[Attribute {
            name: "value",
            summary: "Status code.",
            required: true,
            values: NONE,
        }],
    },
    Tag {
        name: "iscookie",
        summary: "Sets a cookie on the response.",
        body: Body::Empty,
        attributes: &[
            Attribute {
                name: "name",
                summary: "Cookie name.",
                required: true,
                values: NONE,
            },
            Attribute {
                name: "value",
                summary: "Cookie value.",
                required: true,
                values: NONE,
            },
            Attribute {
                name: "domain",
                summary: "Domain the cookie is sent to.",
                required: false,
                values: NONE,
            },
            Attribute {
                name: "path",
                summary: "Path the cookie is sent for.",
                required: false,
                values: NONE,
            },
            Attribute {
                name: "maxAge",
                summary: "Lifetime in seconds.",
                required: false,
                values: NONE,
            },
            Attribute {
                name: "secure",
                summary: "Only send over HTTPS.",
                required: false,
                values: &["true", "false"],
            },
            Attribute {
                name: "comment",
                summary: "Cookie comment.",
                required: false,
                values: NONE,
            },
            Attribute {
                name: "version",
                summary: "Cookie version.",
                required: false,
                values: NONE,
            },
        ],
    },
    Tag {
        name: "isobject",
        summary: "Records a product impression for active data.",
        body: Body::Inline,
        attributes: &[
            Attribute {
                name: "object",
                summary: "Product or content being viewed.",
                required: true,
                values: NONE,
            },
            Attribute {
                name: "view",
                summary: "Context the object is shown in.",
                required: true,
                values: &[
                    "none",
                    "searchhit",
                    "recommendation",
                    "setproduct",
                    "detail",
                ],
            },
        ],
    },
    Tag {
        name: "isselect",
        summary: "Renders a select element from an iterator.",
        body: Body::Empty,
        attributes: &[
            Attribute {
                name: "name",
                summary: "Form field name.",
                required: true,
                values: NONE,
            },
            Attribute {
                name: "iterator",
                summary: "Collection backing the options.",
                required: true,
                values: NONE,
            },
            Attribute {
                name: "value",
                summary: "Expression giving each option value.",
                required: false,
                values: NONE,
            },
            Attribute {
                name: "description",
                summary: "Expression giving each option label.",
                required: false,
                values: NONE,
            },
            Attribute {
                name: "condition",
                summary: "Expression selecting the preselected option.",
                required: false,
                values: NONE,
            },
            Attribute {
                name: "encoding",
                summary: "Output encoding.",
                required: false,
                values: ENCODINGS,
            },
        ],
    },
    Tag {
        name: "isactivedatahead",
        summary: "Loads the active data tracking libraries. Goes in <head>.",
        body: Body::Empty,
        attributes: NONE_ATTRIBUTES,
    },
    Tag {
        name: "isactivedatacontext",
        summary: "Reports the category context of the page.",
        body: Body::Empty,
        attributes: &[Attribute {
            name: "category",
            summary: "Deepest category being browsed.",
            required: true,
            values: NONE,
        }],
    },
    Tag {
        name: "isanalyticsoff",
        summary: "Suppresses the analytics snippet on this page.",
        body: Body::Empty,
        attributes: NONE_ATTRIBUTES,
    },
    Tag {
        name: "isapplepay",
        summary: "Renders the Apple Pay button.",
        body: Body::Empty,
        attributes: NONE_ATTRIBUTES,
    },
    Tag {
        name: "isbuynow",
        summary: "Express checkout button for one product (B2C Commerce Payments).",
        body: Body::Empty,
        attributes: NONE_ATTRIBUTES,
    },
    Tag {
        name: "ispayment",
        summary: "Payment methods for the basket (B2C Commerce Payments).",
        body: Body::Empty,
        attributes: NONE_ATTRIBUTES,
    },
    Tag {
        name: "ispaymentmessages",
        summary: "Instalment and credit callouts (B2C Commerce Payments).",
        body: Body::Empty,
        attributes: NONE_ATTRIBUTES,
    },
];

const NONE_ATTRIBUTES: &[Attribute] = &[];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_the_tags_a_template_actually_uses() {
        for name in ["isif", "isloop", "isinclude", "isprint", "isset", "iscache"] {
            assert!(tag(name).is_some(), "{name} is missing");
        }
    }

    #[test]
    fn has_no_namespaced_html_lookalikes() {
        assert!(tags().iter().all(|tag| !tag.name.contains(':')));
    }

    #[test]
    fn marks_the_required_attributes_of_isslot() {
        let required: Vec<&str> = tag("isslot")
            .unwrap()
            .attributes
            .iter()
            .filter(|attribute| attribute.required)
            .map(|attribute| attribute.name)
            .collect();
        assert_eq!(required, ["id", "description", "context"]);
    }
}
