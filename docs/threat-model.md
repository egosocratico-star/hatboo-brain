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
| T6 | Coste descontrolado (bucle, tokens, RAM) | Presupuestos del Plan + `FailureClass::Truncado` aborta. La RAM dejó de ser una puerta el 05-10, por decisión de él: el Governor **ya no niega un turno** por falta de sitio —aprieta la ventana si el peldaño de abajo cuadra, y si no corre con el margen gastado dejando `Ajuste::SinMargen` en el aviso—; lo que sigue descartando es un modelo **sin medición**, donde no hay cuenta que hacer. Ir justo de RAM lo paga la máquina y se ve en la nota del Brain | `tests/recovery.rs`, `tests/resources.rs`, batería `--decisiones` | **aceptado y observable** (no bloqueante) desde el 05-10 |
| T7 | SSRF por búsqueda web | El destino del `GET` es fijo y **no se siguen redirects**; además el `uddg=` de cada resultado pasa por `es_destino_aceptable` (esquema http/https, y fuera bucle invertido, privadas, link-local/metadata, multicast, `.localhost`/`.local`/`.internal` y `file://`) | `hatboo/src-tauri/src/web.rs`: `un_resultado_no_puede_apuntar_a_la_maquina` y `el_parseo_descarta_los_destinos_que_no_pueden_ser_fuentes` | **cerrado el 06-10** en el producto. El navegador en sí sigue sin existir: esto es la puerta que había que cerrar antes de hablar de G.4 |
| T8 | Supply chain | `Cargo.lock` versionado (ambos repos), CI en `windows-latest` con clippy `-D warnings` + pruebas + batería | la propia CI | **parcial**. Las actions están fijadas por **tag** (`@v5`, `@v2`), no por commit SHA; no hay audit ni SBOM |
| T9 | Update secuestrado | — | — | **pendiente** (hitó M5). Hay release por tag con instaladores, sin firma ni rollback verificado |
| T10 | `HATBOO.md` malicioso | El crate no lo deja entrar sin hash aprobado, y el hash **es SHA-256** desde el 04-10 (antes FNV-1a: identificaba linaje, no firmaba, y quien pudiera escribir el archivo podía calcular el hash de otro contenido). El de `plan_hash` y el de la caché siguen en FNV a propósito: no deciden permisos | `project/hatboo_md.rs`: `sin_aprobar_no_entra`, `aprobar_un_hash_y_que_no_cambie_es_lo_unico_que_activa`, `cambiar_el_archivo_apaga_hasta_volver_a_aprobar`; `observability::hash::sha256_coincide_con_el_estandar` | **hecho** |
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
