//! El bloque dinámico, separado del system (§XII del Canon). Es puro: recibe
//! piezas ya presupuestadas y devuelve texto.

use crate::context::Armado;
use crate::planner::Plan;

#[derive(Debug, Clone, PartialEq)]
pub struct ContextoArmado {
    /// El system prompt estable, byte a byte.
    pub system: String,
    /// El bloque dinámico ya montado.
    pub dinamico: String,
    /// Cuánto se quedó fuera, y qué.
    pub rechazado: Vec<String>,
    pub tokens_dinamico: u32,
    /// Hash del system: si dos turnos del mismo plan dan distinto, hay algo
    /// variable metido en la parte estable.
    pub hash_system: String,
}

/// El orden del turno: system → dinámico → mensaje actual. `pregunta` es lo que
/// el usuario acaba de escribir, y no va en `<datos>` porque es instrucción.
pub fn texto_del_turno(armado: &ContextoArmado, pregunta: &str) -> String {
    if armado.dinamico.is_empty() {
        pregunta.to_string()
    } else {
        format!("{}\n\n{}", armado.dinamico, pregunta)
    }
}

/// El presupuesto de contexto que cabe en el Plan firmado.
pub fn presupuesto_efectivo(plan: &Plan) -> u32 {
    plan.context_budget_tokens
}

impl Armado {
    /// Lo que el proveedor va a recibir.
    pub fn a_contexto(&self, system: String) -> ContextoArmado {
        ContextoArmado {
            hash_system: crate::prompt::system::hash_prefijo(&system),
            system,
            dinamico: self.texto(),
            rechazado: self.rechazadas.clone(),
            tokens_dinamico: self.tokens,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{armar, Pieza, Prioridad};
    use crate::prompt::{Identidad, build_system, Estimador};

    fn plan() -> Plan {
        Plan::firmar(crate::planner::plan::Firma {
            level: crate::api::vocab::Level::N2,
            intent: crate::api::vocab::Intent::Modify,
            model: "gemma3:1b".into(),
            provider: "ollama".into(),
            execution_target: crate::api::vocab::ExecutionTarget::Local,
            num_ctx: 4096,
            thinking: crate::api::vocab::ThinkingLevel::Off,
            tools: vec!["read_file".into()],
            output_contract: crate::api::vocab::OutputContract::Patch,
            verification: crate::api::vocab::VerificationMode::Determinista,
            reason: "prueba".into(),
        })
    }

    #[test]
    fn el_mismo_plan_da_el_mismo_system() {
        let p = plan();
        let s1 = build_system(&Identidad::default(), "work", &p.tools, None, None, None, "Responde en español.");
        let s2 = build_system(&Identidad::default(), "work", &p.tools, None, None, None, "Responde en español.");
        let a = armar(vec![Pieza::nueva(Prioridad::Objetivo, "o", "arreglar x")], 100, &Estimador);
        let c1 = a.a_contexto(s1.clone());
        let c2 = a.a_contexto(s2.clone());
        assert_eq!(c1.hash_system, c2.hash_system);
        assert_eq!(c1.system, s1);
    }

    #[test]
    fn el_turno_pone_el_dinamico_antes_de_la_pregunta() {
        let a = armar(
            vec![
                Pieza::nueva(Prioridad::Objetivo, "o", "objetivo"),
                Pieza::nueva(Prioridad::ArchivoNombrado, "archivo:a.rs", "contenido"),
            ],
            1000,
            &Estimador,
        );
        let c = a.a_contexto("SISTEMA".into());
        let t = texto_del_turno(&c, "arregla el error");
        assert!(t.contains("objetivo"));
        assert!(t.contains("<datos origen=\"archivo:a.rs\">"));
        assert!(t.ends_with("arregla el error"));
        assert_eq!(c.rechazado.len(), 0);
    }

    #[test]
    fn sin_dinamico_la_pregunta_va_sola() {
        let c = ContextoArmado {
            system: "s".into(),
            dinamico: String::new(),
            rechazado: vec![],
            tokens_dinamico: 0,
            hash_system: "h".into(),
        };
        assert_eq!(texto_del_turno(&c, "hola"), "hola");
    }

    #[test]
    fn lo_que_no_cabe_queda_constado() {
        let a = armar(
            vec![Pieza::nueva(Prioridad::Resto, "resto:gordo", "x".repeat(4000))],
            10,
            &Estimador,
        );
        let c = a.a_contexto("s".into());
        assert_eq!(c.rechazado, vec!["resto:gordo".to_string()]);
        assert!(c.dinamico.is_empty());
    }
}
