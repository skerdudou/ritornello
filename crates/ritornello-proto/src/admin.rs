use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "req", content = "arg")]
pub enum AdminReq {
    /// Actif d'IHM du plugin (`"ui.js"`, `"ui.css"`). Le chemin est **opaque**
    /// pour le cœur : c'est le plugin qui décide ce qu'il expose.
    GetAsset(String),
    /// Catalogue i18n du plugin dans la langue courante, à plat.
    GetCatalog,
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
    /// `body: None` = chemin inconnu du plugin (le cœur répond 404). Le `mime`
    /// est fourni par le plugin : le cœur ne déduit rien d'une extension.
    Asset { mime: String, body: Option<String> },
    Catalog(serde_json::Value),
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
    fn request_getasset_roundtrip() {
        let r = AdminRequest { id: 1, req: AdminReq::GetAsset("ui.js".into()) };
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, r#"{"id":1,"req":"GetAsset","arg":"ui.js"}"#);
        let back: AdminRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.req, AdminReq::GetAsset("ui.js".into()));
    }

    #[test]
    fn request_getcatalog_roundtrip() {
        let r = AdminRequest { id: 2, req: AdminReq::GetCatalog };
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, r#"{"id":2,"req":"GetCatalog"}"#);
        let back: AdminRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.req, AdminReq::GetCatalog);
    }

    #[test]
    fn resultat_asset_roundtrip_present_et_absent() {
        for r in [
            AdminResult::Asset { mime: "text/javascript".into(), body: Some("export default 1".into()) },
            // `None` est la reponse normale a un chemin inconnu : le coeur la
            // traduit en 404 sans avoir a interpreter le chemin.
            AdminResult::Asset { mime: "text/plain".into(), body: None },
        ] {
            let json = serde_json::to_string(&AdminResponse { id: 3, result: r.clone() }).unwrap();
            let back: AdminResponse = serde_json::from_str(&json).unwrap();
            assert_eq!(back.result, r);
        }
    }

    #[test]
    fn resultat_catalog_roundtrip() {
        let r = AdminResult::Catalog(serde_json::json!({ "btn_save": "Enregistrer" }));
        let json = serde_json::to_string(&AdminResponse { id: 4, result: r.clone() }).unwrap();
        let back: AdminResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.result, r);
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
    fn response_set_roundtrip() {
        let r = AdminResponse { id: 4, result: AdminResult::Set { ok: false, error: Some("nope".into()) } };
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, r#"{"id":4,"result":{"kind":"Set","data":{"ok":false,"error":"nope"}}}"#);
        let back: AdminResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.result, AdminResult::Set { ok: false, error: Some("nope".into()) });
    }
}
