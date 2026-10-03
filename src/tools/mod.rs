//! Las tools que el producto ofrece, y la puerta que decide si una llamada cabe.
//!
//! §XIII del Canon: `allowed(plan, tool, policy)` **no** vive en el prompt. El
//! modelo solo ve `plan.tools`; todo lo demás se rechaza en código y cuenta
//! contra el presupuesto de acciones.

pub mod gating;

pub use gating::{Deciso, Puerta};

use crate::api::request::ToolInfo;
use crate::api::vocab::ToolId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DefinicionTool {
    pub id: ToolId,
    /// Escribe, borra o ejecuta. Manda sobre `max_write_actions`.
    #[serde(default)]
    pub escribe: bool,
    #[serde(default)]
    pub descripcion: String,
    /// JSON Schema de argumentos. Se le pasa al proveedor tal cual.
    #[serde(default)]
    pub argumentos: serde_json::Value,
}

impl DefinicionTool {
    pub fn info(&self) -> ToolInfo {
        ToolInfo {
            id: self.id.clone(),
            escribe: self.escribe,
            descripcion: self.descripcion.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", default)]
pub struct Herramientas {
    pub version: u32,
    #[serde(default)]
    pub tools: Vec<DefinicionTool>,
}

impl Herramientas {
    pub fn desde_json(texto: &str) -> Result<Herramientas, serde_json::Error> {
        serde_json::from_str(texto)
    }

    /// Las tools versionadas con el crate (`config/tools.json`), para un producto
    /// sin directorio de config. Igual que `Reglas::empotradas`: sin archivo no se
    /// finge un catálogo vacío, se dice que falló.
    pub fn empotradas() -> Result<Herramientas, serde_json::Error> {
        const JSON: &str = include_str!("../../config/tools.json");
        serde_json::from_str(JSON)
    }

    pub fn disponibles(&self) -> &[DefinicionTool] {
        &self.tools
    }

    pub fn find(&self, id: &str) -> Option<&DefinicionTool> {
        self.tools.iter().find(|t| t.id == id)
    }

    pub fn ids(&self) -> Vec<ToolId> {
        self.tools.iter().map(|t| t.id.clone()).collect()
    }

    pub fn escribe(&self, id: &str) -> bool {
        self.find(id).map(|t| t.escribe).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn las_tools_embebidas_no_salen_vacias() {
        let h = Herramientas::empotradas().expect("config/tools.json embebido");
        assert!(!h.disponibles().is_empty(), "el catálogo embebido no puede no tener tools");
        assert!(h.escribe("write_file"), "write_file tiene que decir que escribe");
        assert!(h.find("read_file").is_some());
    }

    #[test]
    fn lee_el_catalogo_y_sabe_cual_escribe() {
        let h = Herramientas::desde_json(
            r#"{"version":1,"tools":[
              {"id":"read_file","escribe":false,"descripcion":"lee"},
              {"id":"write_file","escribe":true,"descripcion":"escribe","argumentos":{"type":"object"}},
              {"id":"run_command","escribe":true}
            ]}"#,
        )
        .unwrap();
        assert_eq!(h.disponibles().len(), 3);
        assert!(h.escribe("write_file"));
        assert!(h.escribe("run_command"));
        assert!(!h.escribe("read_file"));
        // Una tool que no existe no escribe: ausencia no es permiso.
        assert!(!h.escribe("borra_todo"));
        assert_eq!(h.ids(), vec!["read_file", "write_file", "run_command"]);
    }
}
