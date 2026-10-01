# HATBOO BRAIN

El cerebro que administra el trabajo alrededor de un modelo. Crate Rust embebible:
no es un chatbot, no es un LLM, no es un servidor.

> El modelo genera posibilidades. El Brain administra el trabajo.

Un pedido entra, sale un **Plan** firmado (`plan_hash`), y alrededor de ese Plan el
Brain decide cuánto contexto, qué tools, qué modelo, qué verificación y qué
recuperación. El producto elige el modelo y ejecuta; el Brain manda la corrida.

```
BrainRequest
  → Fast Path ─ Some ──────────────┐
       └ None → Decision Engine ────┤
                                    ↓
  Planner + Governor → PLAN firmado (schema 6)
  → Contexto + Tool Gate + Selector
  → Prompt Engine → ModelProvider → Ejecutor del producto
  → Verificar ─ Pass → BrainResult
              └ Fail → Recuperación por clase → Plan nuevo | retry | abort
```

## Uso

```rust
use hatboo_brain::prelude::*;
use std::sync::Arc;

let brain = Brain::nuevo(Montaje::de_proveedor(proveedor))?;
let r = brain.run(&BrainRequest::nuevo("mi-producto", Mode::CHAT, "hola")).await?;
println!("{} — {}", r.output.texto, r.plan.reason);
```

`Montaje::de_proveedor` es lo mínimo para arrancar. Un producto real inyecta además
registry, reglas, catálogo de tools, sonda de RAM, contador de tokens y los
ejecutores: `Brain` no adivina rutas de sistema ni mide hardware por su cuenta.

Cinco contratos que no se rompen entre versiones: Request · Decision · Plan ·
Provider · Verification.

## Qué manda y qué no

| El crate hace | El crate **no** hace |
|---|---|
| Decidir nivel (N0–N3), intent, riesgo, contrato, verificación | Ejecutar tools ni comandos (los pide) |
| Firmar el Plan y comprobar sus siete invariantes | Hablar con la UI |
| Elegir modelo entre lo que cabe y la policy permite | Guardar memoria entre conversaciones |
| Repartir contexto, tools y reintentos | Relajar el sandbox ni los 4 niveles de aprobación |
| Verificar y classificar el fallo | Afirmar nada que no esté medido |

El prompt no amplía permisos: la defensa dura es Tool Gate + sandbox + approval +
redacción. Un `HATBOO.md` sin hash aprobado no entra, y un `<datos>` sale escapado.

## Proveedores

`ollama`, `openai`, `anthropic` y `generic` (cualquier puerta compatible con
OpenAI) están implementados; los tres de API van detrás de features de Cargo y son
opcionales en tiempo de compilación. Las claves las pone el producto y nunca se
guardan en el crate: los errores se redactan antes de salir, porque un 401 suele
reenviar la clave en el cuerpo.

## Configuración

El producto dice dónde está el directorio de config; se leen tres archivos
opcionales:

- `brain-rules.json` — señales y routing. Los nombres de señal, nivel y contrato
  son los del crate (español): `has_file_path`, `N2`, `patch`. `text` se acepta
  como alias de `texto` porque el Canon escribe los contratos en inglés.
- `tools.json` — el catálogo con lo que ofrece el producto, marcando cuáles
  escriben (`max_write_actions` manda sobre esas).
- `models.json` — lo **medido** en esta máquina. Si no está, se usa
  `models.example.json` y `Cargada::models_desde_ejemplo` queda en `true` para
  que el producto pueda decirlo: las cifras del ejemplo no son de esta máquina.

`config/models.example.json` lleva RAM residente medida con Ollama 0.35 sobre
8,45 GB sin GPU (Fase 0). Donde solo hay 2048 medido no hay 4096: el Governor
descarta ese escalón y lo dice, en vez de extrapolar el KV cache.

## Medir

```bash
cargo run --release --bin brain-bench -- --sondear
cargo run --release --bin brain-bench -- --suite benchmarks/base.json \
    --modelo <modelo> --reps 3 --salida benchmarks/resultados/brain.json
```

`--sondear` carga cada modelo local a 2048/4096/8192, lee la RAM de `/api/ps` y
prueba una tool call y un `think` reales; escribe `models.sondeado.json`.
`--correr` pasa la suite por el Brain y ejecuta el `check` determinista de cada
entrada sobre una copia limpia del fixture. `--modo baseline` manda el mismo
system prompt sin administración, que es la línea base que pide §9.

Lo que no se pudo comprobar se reporta como `sin_comprobar`, nunca como acierto.

## Estado

Compila y pasan las pruebas: 196 unitarias del crate + 9 ficheros de integración
de §10 (planner, decision, context, resources, prompt, verification, security,
recovery, golden). Las fases 6 (logprobs) y 7 (backend de decisión tipo Laya) están
detrás de flags apagados y sin implementar.

Pendiente de medir, no de escribir: la comparación de §9 contra la línea base real
necesita correr el arnés con un proveedor cargado. Las cifras de RAM de este repo
son de una máquina; en otra hay que volver a sondear.

## Pruebas

```bash
cargo test                    # todo
cargo test --lib              # el crate solo
cargo run --bin brain-bench -- --ayuda
```

## Licencia

MIT.
