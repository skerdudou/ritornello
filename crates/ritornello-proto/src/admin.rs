use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "req", content = "arg")]
pub enum AdminReq {
    /// A UI asset of the plugin (`"ui.js"`, `"ui.css"`). The path is **opaque**
    /// to the core: the plugin decides what it exposes.
    GetAsset(String),
    /// The plugin's i18n catalog, flattened.
    ///
    /// `Some(lang)` asks for **that** language, whatever the plugin's current
    /// locale; `None` keeps the historical behaviour (the current one).
    ///
    /// Carrying the language is what lets the HTTP answer be `immutable`: the
    /// URL then fully determines the content. A `lang` used only as a cache
    /// key would let a stale entry serve another language after a locale
    /// change — the promise would be a lie.
    GetCatalog(Option<String>),
    GetData,
    SetData(serde_json::Value),
    /// Liveness probe: the plugin answers `Pong` without touching its state or
    /// taking any lock. Lets the core tell "busy" (a long `set_data` holds the
    /// lock) from "dead" (socket closed).
    Ping,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminRequest {
    pub id: u64,
    /// Budget granted by the core, in milliseconds, **decided by the nature of
    /// the request**: an in-memory asset does not get the budget of a network
    /// mount. The server enforces it itself and answers `Expired` at the
    /// deadline; absent = no cap on the server side, the client keeps its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<u64>,
    #[serde(flatten)]
    pub req: AdminReq,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum AdminResult {
    /// `body: None` = path unknown to the plugin (the core answers 404). The
    /// `mime` is supplied by the plugin: the core infers nothing from an
    /// extension.
    Asset { mime: String, body: Option<String> },
    Catalog(serde_json::Value),
    Data(serde_json::Value),
    Set { ok: bool, error: Option<String> },
    Pong,
    /// The plugin **is alive** but did not meet the budget (processing or
    /// waiting for the lock). Distinct from no answer at all: here it is the
    /// plugin that says so.
    Expired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminResponse {
    pub id: u64,
    pub result: AdminResult,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_getasset_roundtrip() {
        let r = AdminRequest { id: 1, deadline_ms: None, req: AdminReq::GetAsset("ui.js".into()) };
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, r#"{"id":1,"req":"GetAsset","arg":"ui.js"}"#);
        let back: AdminRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.req, AdminReq::GetAsset("ui.js".into()));
    }

    #[test]
    fn request_getcatalog_roundtrip() {
        let r = AdminRequest { id: 2, deadline_ms: None, req: AdminReq::GetCatalog(None) };
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, r#"{"id":2,"req":"GetCatalog","arg":null}"#);
        let back: AdminRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.req, AdminReq::GetCatalog(None));
    }

    #[test]
    fn request_getcatalog_with_a_language_roundtrip() {
        // The language must be **obeyed**, not merely used as a cache key: it
        // travels on the wire so the plugin can honour it.
        let r = AdminRequest { id: 2, deadline_ms: None, req: AdminReq::GetCatalog(Some("fr".into())) };
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, r#"{"id":2,"req":"GetCatalog","arg":"fr"}"#);
        let back: AdminRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.req, AdminReq::GetCatalog(Some("fr".into())));
    }

    #[test]
    fn asset_result_roundtrip_present_and_absent() {
        for r in [
            AdminResult::Asset { mime: "text/javascript".into(), body: Some("export default 1".into()) },
            // `None` is the normal answer to an unknown path: the core turns it
            // into a 404 without having to interpret the path.
            AdminResult::Asset { mime: "text/plain".into(), body: None },
        ] {
            let json = serde_json::to_string(&AdminResponse { id: 3, result: r.clone() }).unwrap();
            let back: AdminResponse = serde_json::from_str(&json).unwrap();
            assert_eq!(back.result, r);
        }
    }

    #[test]
    fn catalog_result_roundtrip() {
        let r = AdminResult::Catalog(serde_json::json!({ "btn_save": "Enregistrer" }));
        let json = serde_json::to_string(&AdminResponse { id: 4, result: r.clone() }).unwrap();
        let back: AdminResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.result, r);
    }

    #[test]
    fn request_setdata_carries_the_opaque_json() {
        let r = AdminRequest { id: 2, deadline_ms: None, req: AdminReq::SetData(serde_json::json!({"stations": []})) };
        let json = serde_json::to_string(&r).unwrap();
        let back: AdminRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, 2);
        assert_eq!(back.req, AdminReq::SetData(serde_json::json!({"stations": []})));
    }

    #[test]
    fn response_set_roundtrip() {
        let r = AdminResponse { id: 4, result: AdminResult::Set { ok: false, error: Some("nope".into()) } };
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, r#"{"id":4,"result":{"kind":"Set","data":{"ok":false,"error":"nope"}}}"#);
        let back: AdminResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.result, AdminResult::Set { ok: false, error: Some("nope".into()) });
    }

    #[test]
    fn a_request_without_deadline_still_parses() {
        // Frames written before this field existed: no `deadline_ms`.
        let back: AdminRequest = serde_json::from_str(r#"{"id":1,"req":"GetCatalog","arg":null}"#).unwrap();
        assert_eq!(back.deadline_ms, None);
        assert_eq!(back.req, AdminReq::GetCatalog(None));
    }

    #[test]
    fn the_deadline_travels_when_present() {
        let r = AdminRequest { id: 7, deadline_ms: Some(1000), req: AdminReq::GetAsset("ui.js".into()) };
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, r#"{"id":7,"deadline_ms":1000,"req":"GetAsset","arg":"ui.js"}"#);
        let back: AdminRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.deadline_ms, Some(1000));
    }

    #[test]
    fn ping_pong_and_expired_roundtrip() {
        let r = AdminRequest { id: 9, deadline_ms: Some(500), req: AdminReq::Ping };
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, r#"{"id":9,"deadline_ms":500,"req":"Ping"}"#);
        for res in [AdminResult::Pong, AdminResult::Expired] {
            let json = serde_json::to_string(&AdminResponse { id: 9, result: res.clone() }).unwrap();
            let back: AdminResponse = serde_json::from_str(&json).unwrap();
            assert_eq!(back.result, res);
        }
    }
}
