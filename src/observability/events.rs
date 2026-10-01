//! Eventos y cancelación. `run_with` empuja tokens y estados por un canal; el
//! Brain no sabe quién los consume (§VII: «el Engine no conoce la UI»).

use super::metrics::Medida;
use crate::api::response::TaskMetrics;
use crate::api::vocab::{FailureClass, Level, ModelId, ToolId};
use crate::planner::Plan;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum BrainEvent {
    /// El plan firmado, tal cual. La UI pinta el `reason` de aquí.
    PlanCreated(Plan),
    ModeloElegido {
        modelo: ModelId,
        porque: String,
        /// `true` si el modelo pedido no cabía y hubo que cambiar.
        desvio: bool,
    },
    /// Trozo de texto del stream.
    Token(String),
    /// Trozo de razonamiento, si el nivel y el modelo lo permiten.
    Reasoning(String),
    /// La verificación de formato falló a mitad de stream: el producto debe
    /// **retirar** lo mostrado y reintentar (§1 del plan).
    StreamRetracted { motivo: String },
    ToolLlamada {
        tool: ToolId,
        args: serde_json::Value,
    },
    /// Tool fuera del Plan: rechazada en código, no descripta ni ejecutada.
    ToolDenegada {
        tool: ToolId,
        contra_presupuesto: bool,
    },
    Verificacion {
        clase: VerificacionEvent,
        motivo: Option<String>,
    },
    Reintento {
        clase: FailureClass,
        intento: u8,
    },
    Escalada {
        desde: Level,
        a: Level,
        modelo: ModelId,
        porque: String,
    },
    Cancelado,
    Completado(TaskMetrics),
}

#[derive(Debug, Clone, Copy)]
pub enum VerificacionEvent {
    Pass,
    Fail,
    /// No es éxito: se reporta aparte.
    Unverifiable,
}

/// El emisor de eventos, sin dependencias de canal: quien consume decide si es un
/// mpsc de tokio, un callback o nada.
pub trait Emitidor: Send + Sync {
    fn emitir(&self, evento: BrainEvent);
}

impl Emitidor for () {
    fn emitir(&self, _e: BrainEvent) {}
}

/// Cancelación sin `tokio-util`: un flag atómico + espera con `Notify`. El estado
/// de la tarea pasa a `Cancelled` desde cualquier punto (§4 del plan).
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    cancelado: Arc<AtomicBool>,
    aviso: Arc<tokio::sync::Notify>,
}

impl CancelToken {
    pub fn nuevo() -> Self {
        Self::default()
    }

    pub fn cancelar(&self) {
        self.cancelado.store(true, Ordering::SeqCst);
        self.aviso.notify_waiters();
    }

    pub fn cancelado(&self) -> bool {
        self.cancelado.load(Ordering::SeqCst)
    }

    /// Resuelve cuando se cancela. Si ya está cancelado, devuelve de inmediato.
    pub async fn espera(&self) {
        loop {
            if self.cancelado() {
                return;
            }
            let esperado = self.aviso.notified();
            if self.cancelado() {
                return;
            }
            esperado.await;
        }
    }

    /// Envuelve una futura: la gana si se cancela antes.
    pub async fn or_cancel<F: std::future::Future>(&self, f: F) -> Result<F::Output, crate::api::error::BrainError> {
        tokio::select! {
            v = f => Ok(v),
            _ = self.espera() => Err(crate::api::error::BrainError::Cancelled),
        }
    }
}

/// Medición de una corrida, para el log y el panel.
#[derive(Debug, Clone)]
pub struct Corrida {
    pub inicio: std::time::Instant,
    pub ttft_ms: Option<u64>,
    pub medida: Medida,
}

impl Corrida {
    pub fn nueva() -> Self {
        Corrida {
            inicio: std::time::Instant::now(),
            ttft_ms: None,
            medida: Medida::default(),
        }
    }

    pub fn anotar_token(&mut self, cuando_ms: u64) {
        if self.ttft_ms.is_none() {
            self.ttft_ms = Some(cuando_ms);
        }
    }

    pub fn metricas(&self, mut m: TaskMetrics) -> TaskMetrics {
        m.duracion_ms = self.inicio.elapsed().as_millis() as u64;
        if m.ttft_ms.is_none() {
            m.ttft_ms = self.ttft_ms;
        }
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelar_es_inmediato_y_repetible() {
        let c = CancelToken::nuevo();
        assert!(!c.cancelado());
        c.cancelar();
        c.cancelar();
        assert!(c.cancelado());
    }

    #[tokio::test]
    async fn espera_devuelve_cuando_se_cancela() {
        let c = CancelToken::nuevo();
        let c2 = c.clone();
        tokio::spawn(async move { c2.cancelar() });
        tokio::time::timeout(std::time::Duration::from_secs(1), c.espera())
            .await
            .expect("el esperado debió resolver");
    }

    #[tokio::test]
    async fn or_cancel_gana_al_futuro() {
        let c = CancelToken::nuevo();
        let c2 = c.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            c2.cancelar();
        });
        let r = c.or_cancel(async {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            1u8
        })
        .await;
        assert!(matches!(r, Err(crate::api::error::BrainError::Cancelled)));
    }

    #[tokio::test]
    async fn or_cancel_deja_pasar_el_resultado() {
        let c = CancelToken::nuevo();
        assert_eq!(c.or_cancel(async { 7u8 }).await.unwrap(), 7);
    }
}
