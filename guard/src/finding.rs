//! A finding: what a layer saw, how often, the action the policy gives it, and a plain-words explanation for the
//! person who sent the file. Explanations name what was found and what happened; they never quote the file.
use crate::policy::{Action, UseCase};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub enum Layer {
    L0,
    L1,
    L2,
    Policy,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Finding {
    pub code: String,
    pub layer: Layer,
    pub action: Action,
    pub count: u64,
    /// A rule or signature name, never content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub explanation: String,
}

impl Finding {
    pub fn new(
        case: &UseCase,
        code: &'static str,
        layer: Layer,
        count: u64,
        detail: Option<String>,
    ) -> Self {
        let action = case.action(code);
        let explanation = explain(code, count, detail.as_deref(), action);
        Finding {
            code: code.to_string(),
            layer,
            action,
            count,
            detail,
            explanation,
        }
    }
}

fn outcome_words(action: Action) -> &'static str {
    match action {
        Action::Allow | Action::Annotate => "",
        Action::Disarm => " The copy sent on has it removed.",
        Action::Quarantine => " The file is held for review.",
        Action::Block => " The file was refused.",
    }
}

pub fn explain(code: &str, count: u64, detail: Option<&str>, action: Action) -> String {
    let what = match code {
        "empty_file" => "The file is empty.".to_string(),
        "size_exceeded" => "The file is larger than this upload accepts.".to_string(),
        "type_not_allowed" => format!(
            "The file is {}, which this upload does not accept.",
            detail.unwrap_or("of another type")
        ),
        "type_mismatch" => format!(
            "The file is not the type it was sent as ({}).",
            detail.unwrap_or("mismatch")
        ),
        "polyglot" => "The file also contains the structure of another file format.".to_string(),
        "trailing_data" => format!("The file has {count} bytes after its end marker."),
        "structure_invalid" => "The file's internal structure could not be read.".to_string(),
        "encrypted_password" => "The PDF is encrypted with a password.".to_string(),
        "encrypted_owner_only" => "The PDF carries owner-password restrictions.".to_string(),
        "active_javascript" => format!("The PDF contains JavaScript ({count})."),
        "active_launch" => {
            format!("The PDF contains an action that starts a program or opens a file ({count}).")
        }
        "active_embedded_file" => format!("The PDF contains embedded files ({count})."),
        "active_open_action" => "The PDF runs an action when it is opened.".to_string(),
        "active_additional_actions" => {
            format!("The PDF contains actions triggered by viewing or form events ({count}).")
        }
        "active_xfa" => "The PDF contains an XFA form.".to_string(),
        "active_rich_media" => format!("The PDF contains rich media or 3D content ({count})."),
        "active_form_submit" => {
            format!("The PDF contains form actions that send or import data ({count}).")
        }
        "active_remote_goto" => format!("The PDF contains links that open other files ({count})."),
        "hidden_layers" => "The PDF has layers hidden by default.".to_string(),
        "hidden_text_invisible" => format!(
            "The PDF has {count} characters drawn invisibly (often a scanned page's text layer)."
        ),
        "hidden_text_white" => format!("The PDF has {count} characters drawn in white."),
        "hidden_text_tiny" => format!("The PDF has {count} characters smaller than 1 point."),
        "hidden_text_offpage" => format!("The PDF has {count} characters placed outside the page."),
        "analysis_truncated" => {
            "The PDF is too large to check every page for hidden text; the rest was not checked."
                .to_string()
        }
        "unicode_tags" => format!(
            "The text contains {count} Unicode tag characters, which can hide instructions."
        ),
        "unicode_bidi" => {
            format!("The text contains {count} characters that reverse text direction.")
        }
        "unicode_zero_width" => format!("The text contains {count} zero-width characters."),
        "metadata_text" => "The file's metadata holds text.".to_string(),
        "malware_signature" => format!(
            "The malware scanner matched a known threat ({}).",
            detail.unwrap_or("signature")
        ),
        "yara_high" | "yara_medium" | "yara_low" => {
            format!(
                "A file-content rule matched ({}).",
                detail.unwrap_or("rule")
            )
        }
        "analysis_failed" => "The file could not be checked within the guard's limits.".to_string(),
        "private_streams" => format!("The PDF holds {count} data streams that no page uses."),
        "image_too_large" => format!("The PDF has {count} images too large to open safely."),
        "streams_unscanned" => {
            format!("The PDF has {count} data streams the guard could not decode and scan.")
        }
        "embedded_archive" => {
            format!("The PDF carries {count} archives (ZIP, 7-Zip, RAR or xz) inside its data.")
        }
        "content_irregular" => format!(
            "The PDF's page content holds {count} operators or values no viewer draws with; the rebuilt copy leaves them out."
        ),
        "render_failed" => {
            "The rebuilt copy could not be drawn within the guard's memory and time limits."
                .to_string()
        }
        "disarm_failed" => "The file could not be rebuilt safely.".to_string(),
        "disarm_residual" => {
            "Active content, unused data or undrawn content remained after rebuilding.".to_string()
        }
        "disarm_text_changed" => {
            "The rebuilt copy's extracted text differs from the original's.".to_string()
        }
        "disarm_text_unavailable" => {
            "The text of the original or the rebuilt copy could not be extracted to compare them."
                .to_string()
        }
        "table_too_large" => {
            let bound = detail.unwrap_or("the table is larger than this upload accepts");
            let mut words = bound.chars();
            match words.next() {
                Some(first) => format!("{}{}.", first.to_uppercase(), words.as_str()),
                None => "The table is larger than this upload accepts.".to_string(),
            }
        }
        "active_macros" => format!("The workbook contains macros ({count})."),
        "active_controls" => format!("The workbook contains ActiveX or form controls ({count})."),
        "embedded_objects" => format!("The workbook contains embedded objects or files ({count})."),
        "external_links" => format!(
            "The workbook takes values from other files or data connections ({count}); the values it last saved are kept."
        ),
        "xml_entities" => format!(
            "The workbook declares XML entities or a document type ({count}), which no workbook needs."
        ),
        "hidden_sheets" => {
            format!("The workbook has hidden sheets ({count}); the copy keeps them, marked hidden.")
        }
        "formulas_without_values" => format!(
            "Formula cells saved without a value ({count}) are empty in the copy; formulas are never run."
        ),
        "exception_applied" => format!(
            "Released by a recorded exception ({}).",
            detail.unwrap_or("approved")
        ),
        other => format!("Finding {other}."),
    };
    format!("{what}{}", outcome_words(action))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explanations_say_what_happened() {
        assert_eq!(
            explain("active_javascript", 2, None, Action::Disarm),
            "The PDF contains JavaScript (2). The copy sent on has it removed."
        );
        assert_eq!(
            explain(
                "malware_signature",
                1,
                Some("Eicar-Test-Signature"),
                Action::Block
            ),
            "The malware scanner matched a known threat (Eicar-Test-Signature). The file was refused."
        );
        assert!(explain("unicode_tags", 9, None, Action::Quarantine).ends_with("held for review."));
        assert_eq!(
            explain(
                "table_too_large",
                1,
                Some("the table has more rows than a spreadsheet can"),
                Action::Block
            ),
            "The table has more rows than a spreadsheet can. The file was refused."
        );
        assert_eq!(
            explain("active_macros", 2, None, Action::Disarm),
            "The workbook contains macros (2). The copy sent on has it removed."
        );
    }
}
