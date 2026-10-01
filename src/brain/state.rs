//! El estado de la tarea. Es mutable y vive **fuera** del `plan_hash`: si entrara
//! en el hash, cada paso invalidaría el plan (§VIII del Canon).

use crate::api::vocab::FailureClass;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Etapa {
    Recibido,
    Perfilado,
    Planificado,
    Preparado,
    Ejecutando,
    Verificando,
    Recuperando,
    Completado,
    Fallido,
    Cancelado,
}

impl Etapa {
    /// Las transiciones que §4 del plan permite desde `Recuperando`.
    pub fn puede_seguir_a(&self, a: &Etapa) -> bool {
        use Etapa as E;
        matches!(
            (self, a),
            (E::Recibido, E::Perfilado)
                | (E::Perfilado, E::Planificado)
                | (E::Planificado, E::Preparado)
                | (E::Preparado, E::Ejecutando)
                | (E::Ejecutando, E::Verificando)
                | (E::Verificando, E::Recuperando)
                | (E::Verificando, E::Completado)
                | (E::Recuperando, E::Preparado)
                | (E::Recuperando, E::Planificado)
                | (E::Recuperando, E::Fallido)
        ) || *a == E::Cancelado
    }

    pub fn es_terminal(&self) -> bool {
        matches!(self, Etapa::Completado | Etapa::Fallido | Etapa::Cancelado)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EstadoTarea {
    pub etapa: Etapa,
    /// Lo que el usuario pidió, en una frase. Prioridad alta en el contexto.
    pub objetivo: String,
    pub completado: Vec<String>,
    pub pendiente: Vec<String>,
    /// Con el porqué: un bloqueo sin explicación es una tarea que se para callada.
    pub bloqueado: Vec<(String, String)>,
    /// Clases de fallo vistas en esta tarea.
    pub fallos: Vec<FailureClass>,
}

impl Default for EstadoTarea {
    fn default() -> Self {
        EstadoTarea {
            etapa: Etapa::Recibido,
            objetivo: String::new(),
            completado: vec![],
            pendiente: vec![],
            bloqueado: vec![],
            fallos: vec![],
        }
    }
}

impl EstadoTarea {
    pub fn nuevo(objetivo: impl Into<String>) -> EstadoTarea {
        EstadoTarea {
            objetivo: objetivo.into(),
            ..Default::default()
        }
    }

    pub fn ir_a(&mut self, a: Etapa) -> bool {
        if self.etapa.puede_seguir_a(&a) {
            self.etapa = a;
            true
        } else {
            false
        }
    }

    pub fn anotar_fallo(&mut self, c: FailureClass) {
        self.fallos.push(c);
    }

    /// El cierre de un N3 (§IX): nada pendiente, blocked explicado y diff en
    /// alcance. Es determinista: no pregunta a un modelo si «se acabó».
    pub fn cierre_ok(&self) -> bool {
        self.pendiente.is_empty() && self.bloqueado.iter().all(|(_, porque)| !porque.is_empty())
    }

    /// Cómo entra al contexto dinámico. Corto: es la pieza de prioridad 1.
    pub fn al_prompt(&self) -> String {
        let mut v = vec![format!("Objetivo: {}", self.objetivo)];
        v.push(format!("Etapa: {:?}", self.etapa));
        if !self.completado.is_empty() {
            v.push(format!("Hecho: {}", self.completado.join("; ")));
        }
        if !self.pendiente.is_empty() {
            v.push(format!("Pendiente: {}", self.pendiente.join("; ")));
        }
        for (cosa, porque) in &self.bloqueado {
            v.push(format!("Bloqueado: {cosa} — {porque}"));
        }
        if !self.fallos.is_empty() {
            v.push(format!(
                "Fallos de esta tarea: {:?}",
                self.fallos
            ));
        }
        v.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use FailureClass as F;

    #[test]
    fn las_transiciones_son_las_del_plan() {
        let mut e = EstadoTarea::nuevo("arreglar main.rs");
        assert_eq!(e.etapa, Etapa::Recibido);
        assert!(e.ir_a(Etapa::Perfilado));
        assert!(!e.ir_a(Etapa::Completado), "no se puede saltar al final");
        assert_eq!(e.etapa, Etapa::Perfilado);
        for a in [
            Etapa::Planificado,
            Etapa::Preparado,
            Etapa::Ejecutando,
            Etapa::Verificando,
            Etapa::Recuperando,
            Etapa::Preparado,
        ] {
            assert!(e.ir_a(a), "{a:?}");
        }
    }

    #[test]
    fn cancelar_vale_desde_cualquiera() {
        let mut e = EstadoTarea::nuevo("x");
        e.ir_a(Etapa::Ejecutando);
        assert!(e.ir_a(Etapa::Cancelado));
        assert!(e.etapa.es_terminal());
    }

    #[test]
    fn el_cierre_pide_nada_pendiente_y_bloqueos_explicados() {
        let mut e = EstadoTarea::nuevo("x");
        assert!(e.cierre_ok(), "vacío es cierre válido");
        e.pendiente.push("otra cosa".into());
        assert!(!e.cierre_ok());
        e.pendiente.clear();
        e.bloqueado.push(("sin credencial".into(), String::new()));
        assert!(!e.cierre_ok(), "un bloqueo mudo no cierra");
        e.bloqueado = vec![("sin credencial".into(), "el usuario no la dio".into())];
        assert!(e.cierre_ok());
    }

    #[test]
    fn el_estado_entra_al_prompt_corto() {
        let mut e = EstadoTarea::nuevo("corregir el error");
        e.ir_a(Etapa::Planificado);
        e.completado.push("leído main.rs".into());
        e.anotar_fallo(F::Formato);
        let p = e.al_prompt();
        assert!(p.contains("Objetivo: corregir el error"));
        assert!(p.contains("Hecho: leído main.rs"));
        assert!(p.contains("Formato"), "{p}");
        assert!(p.chars().count() < 400);
    }
}
