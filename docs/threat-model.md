# Modelo de amenazas

Derivado de §8 del Plan maestro v2. Cada fila dice **dónde** se corta y **qué
prueba** lo sostiene. El estado es honesto: `hecho` significa que hay código y
hay prueba; `pendiente` significa que el plan lo pide y todavía no está.

| # | Amenaza | Dónde se corta | Prueba | Estado |
|---|---|---|---|---|
| T1 | Inyección por archivo, web, `HATBOO.md` o descripción de tool | `prompt/escape.rs` (bloque `<datos>` escapado, origen saneado) + Tool Gate: el modelo no ve tools que el Plan no firmó | `tests/prompt.rs`, `tests/verification.rs`, `intenta_romper` | **hecho** |
| T2 | Tool fuera del Plan | `planner/validation.rs::ToolNotAllowed`, y en runtime `tools_del_plan` | `tests/planner.rs` (`rechaza_lo_que_no_debe_pasar`) | **hecho** |
| T3 | Escape de ruta (`..`, symlink, UNC, 8.3) | En el producto: `hatboo/src-tauri/src/agent/tools/mod.rs` canonicaliza contra la raíz del proyecto y devuelve `PathEscape` | tests de tools del producto | **hecho en Hatboo**, fuera del crate (el crate no toca disco) |
| T4 | Secreto en log, export o petición a una API | `security/redact.rs` (texto y errores), y `context::fuera_del_equipo` para lo sensible por pieza | `tests/security.rs` (una `sk-ant-…` sale `sk-ant-***`) | **hecho** |
| T5 | Auto-ejecución de comandos | El Brain pide, no ejecuta; `run_command` en Hatboo está **apagado por defecto** (`state.rs`) y pide aprobación siempre que no esté Acceso total | tests de tools y de aprobación del producto | **hecho en Hatboo**. Sandbox de *proceso* no: `cwd` dentro del proyecto no aísla |
| T6 | Coste descontrolado (bucle, tokens, RAM) | Presupuestos del Plan + `FailureClass::Truncado` aborta + Governor descarta sin medida | `tests/recovery.rs`, `tests/resources.rs`, batería `--decisiones` | **hecho** |
| T7 | SSRF por búsqueda web | — | — | **pendiente**. `web.rs` llama a un endpoint fijo con tiempo de espera de 8 s, pero **no** hay allowlist de hosts y reqwest sigue redirects por defecto. Mientras el único destino sea ese endpoint, la superficie es su respuesta; en cuanto se pueda buscar una URL arbitraria, esto es un hueco real |
| T8 | Supply chain | `Cargo.lock` versionado (ambos repos), CI en `windows-latest` con clippy `-D warnings` + pruebas + batería | la propia CI | **parcial**. Las actions están fijadas por **tag** (`@v5`, `@v2`), no por commit SHA; no hay audit ni SBOM |
| T9 | Update secuestrado | — | — | **pendiente** (hitó M5). Hay release por tag con instaladores, sin firma ni rollback verificado |
| T10 | `HATBOO.md` malicioso | El crate no lo deja entrar sin hash aprobado (trust), **pero el hash es FNV-1a de 64 bits**: identifica linaje, no firma. Un atacante que pueda escribir el archivo puede calcular el hash de otro contenido | `project/hatboo_md.rs`: `sin_aprobar_no_entra`, `aprobar_un_hash_y_que_no_cambie_es_lo_unico_que_activa`, `cambiar_el_archivo_apaga_hasta_volver_a_aprobar` | **pendiente de SHA-256**, que §12 del plan pone en esta semana |
| T11 | XSS en la vista previa de HTML | No es de este crate: lo decide el producto al renderizar | — | **sin verificar aquí**. Hay que comprobar CSP y `sandbox` del iframe en Hatboo antes de afirmar nada |
| T12 | Ollama escuchando en la red | Aviso en el onboarding | — | **pendiente** (M1 pide el onboarding con sondeo) |
| T13 | `brain-improve` tocando el runtime | Zona intocable + prueba en CI | — | **no aplica todavía**: la Fase 8 (auto-mejora) no está construida |

## Lo que se sigue perdiendo aunque no haya amenaza

- **SQLite sin cifrar en reposo.** Decisión abierta (§11.9 del plan). Los secretos
  no están en la base —viven en el llavero del SO—, pero el contenido de los
  chats sí.
- **Sin memoria entre conversaciones** por decisión cerrada, no por olvido.

## Regla para añadir una fila

Amenaza nueva ⇒ prueba nueva. Una mitigación sin prueba que la rompa se escribe
como `pendiente`, no como `hecho`.
