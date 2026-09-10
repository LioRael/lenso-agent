//! Safe navigation derived from the configured App origin and returned Issue IDs.
use super::tools;
use serde_json::{Value, json};

pub(super) fn present(origin: &str, result: &mut tools::ExecuteResponse) {
    let Ok(mut body) = serde_json::from_str::<Value>(&result.content) else {
        return;
    };
    let mut links = Vec::new();
    if let Some(items) = body.get("items").and_then(Value::as_array) {
        links.extend(items.iter().filter_map(|item| issue_link(origin, item)));
    } else if let Some(link) = issue_link(origin, &body) {
        links.push(link);
    }
    if links.is_empty() {
        return;
    }
    // Keep original domain fields intact. Links carry no identity or authorization.
    if let Some(object) = body.as_object_mut() {
        object.insert("_links".into(), Value::Array(links));
        if let Ok(content) = serde_json::to_string_pretty(&body) {
            result.content = content;
        }
    }
}
fn issue_link(origin: &str, issue: &Value) -> Option<Value> {
    let id = issue.get("issue_id")?.as_str()?;
    let organization = issue.get("organization_id")?.as_str()?;
    let title = issue.get("title")?.as_str()?;
    let mut url = reqwest::Url::parse(origin).ok()?;
    url.set_path("/projects");
    url.query_pairs_mut()
        .append_pair("organization_id", organization)
        .append_pair("issue", id);
    Some(json!({"title":title,"url":url.as_str(),"issue_id":id}))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn links_use_configured_origin_and_encode_untrusted_identifiers() {
        let link = issue_link(
            "https://app.example",
            &json!({"issue_id":"a&issue=other#x", "organization_id":"org?x", "title":"An issue"}),
        )
        .unwrap();
        let url = reqwest::Url::parse(link["url"].as_str().unwrap()).unwrap();
        assert_eq!(url.origin().ascii_serialization(), "https://app.example");
        assert_eq!(
            url.query_pairs().collect::<Vec<_>>(),
            vec![
                ("organization_id".into(), "org?x".into()),
                ("issue".into(), "a&issue=other#x".into())
            ]
        );
        assert!(issue_link("https://app.example", &json!({"issue_id":"x"})).is_none());
    }
}
