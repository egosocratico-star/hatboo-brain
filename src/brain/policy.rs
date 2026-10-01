//! La matriz policy × approval × permiso de API (§15.2 del plan). Vive en código
//! porque es un invariante, no una preferencia: un prompt no puede cambiarla.

use crate::api::vocab::{ApprovalLevel, ExecutionPolicy, ExecutionTarget, Risk};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Permiso {
    /// Se ejecuta directamente.
    Permitida,
    /// El producto tiene que preguntar antes de ejecutar.
    RequiereAprobacion,
    /// Prohibida por la política o el nivel de aprobación, con el motivo.
    Prohibida(&'static str),
}

impl Permiso {
    pub fn se_puede_ejecutar(&self) -> bool {
        matches!(self, Permiso::Permitida)
    }
}

/// Qué hace falta para que una herramienta se ejecute. El Brain **interseca**:
/// nunca una tool que el approval no permita, y nunca API si la policy es
/// `local_only`. La aprobación la decide el producto; aquí solo se formula la
/// pregunta correcta.
pub fn permiso(
    escribe: bool,
    approval: ApprovalLevel,
    risk: Risk,
    dentro_del_plan: bool,
) -> Permiso {
    if !dentro_del_plan {
        return Permiso::Prohibida("la herramienta no está en el Plan");
    }
    if !escribe {
        return match approval {
            ApprovalLevel::AskAlways if risk == Risk::High => Permiso::RequiereAprobacion,
            _ => Permiso::Permitida,
        };
    }
    match approval {
        ApprovalLevel::AskAlways => Permiso::RequiereAprobacion,
        ApprovalLevel::ApproveForMe => Permiso::RequiereAprobacion,
        // En `auto_sandbox` el producto ya limitó el alcance: la escritura vale
        // dentro del proyecto y el sandbox, no fuera.
        ApprovalLevel::AutoSandbox if risk == Risk::High => Permiso::RequiereAprobacion,
        ApprovalLevel::AutoSandbox => Permiso::Permitida,
        ApprovalLevel::FullAccess => Permiso::Permitida,
    }
}

/// ¿Este destino de ejecución está permitido por la policy? `consentimiento` es
/// el sí explícito del usuario a que algo salga del equipo: sin él, ninguna
/// policy que no sea `cloud_only` autoriza una API.
pub fn destino_permitido(
    policy: ExecutionPolicy,
    target: ExecutionTarget,
    hay_local_elegible: bool,
    consentimiento: bool,
) -> bool {
    use ExecutionPolicy as P;
    match target {
        ExecutionTarget::Local => !matches!(policy, P::CloudOnly),
        ExecutionTarget::Api => match policy {
            P::LocalOnly => false,
            P::CloudOnly => consentimiento,
            P::LocalPreferred => !hay_local_elegible && consentimiento,
            P::Balanced | P::CloudAllowed => consentimiento,
        },
        // El fallback respeta lo que permitía la policy original.
        ExecutionTarget::Fallback => !matches!(policy, P::LocalOnly) && consentimiento,
    }
}

/// El texto que el producto muestra para pedir el consentimiento. No se puede
/// asumir por defecto: `local_preferred` es el default y su salida a API cuesta
/// datos del usuario.
pub fn pregunta_de_consentimiento(policy: ExecutionPolicy, modelo: &str) -> Option<String> {
    use ExecutionPolicy as P;
    match policy {
        P::LocalOnly => None,
        P::LocalPreferred | P::Balanced | P::CloudAllowed | P::CloudOnly => Some(format!(
            "«{modelo}» corre fuera de tu máquina. ¿Permitido para este pedido?"
        )),
    }
}

/// ¿Qué policy implica que hay que preguntar? (Para el log y el panel: una API a
/// la que se fue sin preguntar es un fallo de producto, no de red.)
pub fn hubo_que_preguntar(policy: ExecutionPolicy, target: ExecutionTarget) -> bool {
    target != ExecutionTarget::Local && !matches!(policy, ExecutionPolicy::LocalOnly)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ExecutionPolicy as P;
    use ExecutionTarget as T;

    #[test]
    fn leer_casi_nunca_pide_permiso_escribir_siempre() {
        assert_eq!(
            permiso(false, ApprovalLevel::AskAlways, Risk::Low, true),
            Permiso::Permitida
        );
        assert_eq!(
            permiso(true, ApprovalLevel::AskAlways, Risk::Low, true),
            Permiso::RequiereAprobacion
        );
        assert_eq!(
            permiso(true, ApprovalLevel::FullAccess, Risk::High, true),
            Permiso::Permitida
        );
        // Auto en sandbox: normal vale, de riesgo alto se pregunta.
        assert_eq!(
            permiso(true, ApprovalLevel::AutoSandbox, Risk::Low, true),
            Permiso::Permitida
        );
        assert_eq!(
            permiso(true, ApprovalLevel::AutoSandbox, Risk::High, true),
            Permiso::RequiereAprobacion
        );
    }

    #[test]
    fn fuera_del_plan_no_hay_nivel_que_lo_salve() {
        for a in [
            ApprovalLevel::AskAlways,
            ApprovalLevel::ApproveForMe,
            ApprovalLevel::AutoSandbox,
            ApprovalLevel::FullAccess,
        ] {
            assert!(matches!(
                permiso(true, a, Risk::Low, false),
                Permiso::Prohibida(_)
            ));
        }
    }

    #[test]
    fn local_only_es_de_verdad() {
        assert!(!destino_permitido(P::LocalOnly, T::Api, false, true));
        assert!(!destino_permitido(P::LocalOnly, T::Fallback, false, true));
        assert!(destino_permitido(P::LocalOnly, T::Local, true, false));
    }

    #[test]
    fn la_api_nunca_va_sin_consentimiento() {
        for p in [P::LocalPreferred, P::Balanced, P::CloudAllowed, P::CloudOnly] {
            assert!(
                !destino_permitido(p, T::Api, false, false),
                "{p:?} dejó salir sin preguntar"
            );
        }
    }

    #[test]
    fn local_preferred_solo_sale_si_no_queda_nada_local() {
        assert!(!destino_permitido(P::LocalPreferred, T::Api, true, true));
        assert!(destino_permitido(P::LocalPreferred, T::Api, false, true));
        // cloud_allowed: sale aunque haya local, si hay consentimiento.
        assert!(destino_permitido(P::CloudAllowed, T::Api, true, true));
    }

    #[test]
    fn cloud_only_no_usa_lo_local() {
        assert!(!destino_permitido(P::CloudOnly, T::Local, true, true));
        assert!(destino_permitido(P::CloudOnly, T::Api, true, true));
    }

    #[test]
    fn la_pregunta_existe_cuando_toca() {
        assert_eq!(pregunta_de_consentimiento(P::LocalOnly, "q"), None);
        let q = pregunta_de_consentimiento(P::LocalPreferred, "gpt-4o-mini").unwrap();
        assert!(q.contains("fuera de tu máquina"), "{q}");
        assert!(hubo_que_preguntar(P::LocalPreferred, T::Api));
        assert!(!hubo_que_preguntar(P::LocalPreferred, T::Local));
        assert!(!hubo_que_preguntar(P::LocalOnly, T::Api));
    }
}
