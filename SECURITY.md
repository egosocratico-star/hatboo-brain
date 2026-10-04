# Seguridad

Qué defiende este crate, qué defiende el producto y qué **no** está defendido
todavía. La tabla de amenazas está en `docs/threat-model.md`; aquí va el cómo.

## Las defensas duras, por orden de lo que corta primero

Ninguna depende de que el modelo se porte bien. El prompt no amplía permisos:
lo que no pasa por el código, no pasa.

1. **Tool Gate** (`src/tools/gating.rs`, `src/planner/validation.rs`). El modelo
   solo ve las tools que el Plan firmó, y el Plan solo firma las que el producto
   ofrece y el nivel permite tocar (`ToolNotAllowed`). `escalate_to` no regala
   tools: escalar de nivel no abre `write_file`.
2. **Presupuesto firmado** (`src/planner/plan.rs`, `armador.rs`). Contexto,
   escrituras, llamadas, reintentos y tiempo van en el Plan y se validan contra
   `num_ctx` antes de llamar a nada. El razonamiento cuenta como salida.
3. **Aprobación** (`ApprovalLevel`, cuatro niveles). La decide y la aplica el
   producto, nunca el Brain: `validate_plan` corta lo que el nivel no permite,
   pero el sí-lo-hago es del usuario. El Brain **no relaja** el nivel por
   convicción propia, y escalar de tier no lo sube.
4. **Redacción** (`src/security/redact.rs`). Toda clave con forma conocida
   (`sk-ant-`, `ghp_`, `hf_`, Bearer, JWT) se tapa antes de salir a log, error o
   API. Los errores de proveedor se redactan **antes** de devolverse: un 401
   suele reenviar la clave en el cuerpo.
5. **Etiquetado de datos ajenos** (`src/prompt/escape.rs`). Lo que viene de
   archivos, web o tools entra en un bloque `<datos>` escapado, con el origen
   saneado. `intenta_romper` es la prueba de que un texto con etiquetas falsas
   no se hace pasar por el sistema.
6. **Sensibilidad por pieza** (`src/context/sources.rs`, `fuera_del_equipo`). Lo
   que parece secreto (`.env*`, `*.pem`, `*.key`, `id_rsa*`, `credentials`,
   `secrets`) **no viaja** si el Plan apunta a una API, y se registra el porqué.
7. **Governor** (`src/resources/governor.rs`). Sin lectura de RAM no se afirma
   que un modelo quepa: se descarta y se dice.

## Lo que este crate no hace, a propósito

- No ejecuta tools ni comandos: los pide. El sandbox de rutas, el tiempo de
  espera de un proceso y el aislamiento son del producto.
- No guarda secretos ni estado entre conversaciones.
- No decide permisos: propone y rinde cuentas (`plan_hash`, `reason`, eventos).
- `HATBOO.md` sin hash aprobado no entra al contexto. Ese hash es **SHA-256**
  desde el 04-10, y a propósito: decide si un contenido llega al prompt. Los
  hashes de linaje (`plan_hash`, `parent_plan_hash`, la firma de la caché) siguen
  en FNV-1a-64, que identifica pero no firma. Ver `docs/threat-model.md`,
  amenaza T10.

## Denegación de servicio y bucles

`max_tool_calls`, `max_write_actions`, `max_retries` y `timeout_s` van firmados
en el Plan; `FailureClass::Truncado` aborta en vez de reintentar con el mismo
techo (dos llamadas al modelo que no podían salir bien); y la escalada cambia de
tier o se detiene, no repite lo mismo.

## Reportar

Abre un issue privado en el repo o escribe a la dirección del `Cargo.toml`. No
se publican reproducciones con datos reales de una máquina de terceros.
