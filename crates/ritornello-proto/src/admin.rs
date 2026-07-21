use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "req", content = "arg")]
pub enum AdminReq {
    GetPage,
    GetData,
    SetData(serde_json::Value),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminRequest {
    pub id: u64,
    #[serde(flatten)]
    pub req: AdminReq,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum AdminResult {
    Page(String),
    Data(serde_json::Value),
    Set { ok: bool, error: Option<String> },
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
    fn request_getpage_roundtrip() {
        let r = AdminRequest { id: 1, req: AdminReq::GetPage };
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, r#"{"id":1,"req":"GetPage"}"#);
        let back: AdminRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, 1);
        assert_eq!(back.req, AdminReq::GetPage);
    }

    #[test]
    fn request_setdata_porte_le_json_opaque() {
        let r = AdminRequest { id: 2, req: AdminReq::SetData(serde_json::json!({"stations": []})) };
        let json = serde_json::to_string(&r).unwrap();
        let back: AdminRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, 2);
        assert_eq!(back.req, AdminReq::SetData(serde_json::json!({"stations": []})));
    }

    #[test]
    fn response_page_roundtrip() {
        let r = AdminResponse { id: 3, result: AdminResult::Page("<h1>x</h1>".into()) };
        let json = serde_json::to_string(&r).unwrap();
        let back: AdminResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, 3);
        assert_eq!(back.result, AdminResult::Page("<h1>x</h1>".into()));
    }

    #[test]
    fn response_set_roundtrip() {
        let r = AdminResponse { id: 4, result: AdminResult::Set { ok: false, error: Some("nope".into()) } };
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, r#"{"id":4,"result":{"kind":"Set","data":{"ok":false,"error":"nope"}}}"#);
        let back: AdminResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.result, AdminResult::Set { ok: false, error: Some("nope".into()) });
    }
}
