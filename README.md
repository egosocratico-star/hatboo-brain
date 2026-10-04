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
  Planner + Governor → PLAN firmado (schema 7)
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

El producto dice dónde está el directorio de config; se leen cuatro archivos
opcionales:

- `brain-rules.json` — señales y routing. Los nombres de señal, nivel y contrato
  son los del crate (español): `has_file_path`, `N2`, `patch`. `text` se acepta
  como alias de `texto` porque el Canon escribe los contratos en inglés.
- `tuning.json` — los pesos de la decisión ponderada por costo (§3). Sin archivo
  los pesos son neutrales (todo a 1,0) y la elección es la de siempre; ponerlos sin
  medir la Fase 3 sería inventarse un coste.
- `tools.json` — el catálogo con lo que ofrece el producto, marcando cuáles
  escriben (`max_write_actions` manda sobre esas).
- `models.json` — lo **medido** en esta máquina. Si no está, se usa
  `models.example.json` y `Cargada::models_desde_ejemplo` queda en `true` para
  que el producto pueda decirlo: las cifras del ejemplo no son de esta máquina.

Las reglas y el catálogo de tools **viajan dentro del crate** (`Reglas::empotradas()`
y `Herramientas::empotradas()`, que leen por `include_str!` los JSON de `config/` y
devuelven error si no parsean, nunca vacío). Ojo con la asimetría: `Cargada::leer()`
con un directorio sin `brain-rules.json` deja `Reglas` **vacías** — no cae al
embebido por su cuenta. Un producto empaquetado que no tiene directorio de config
tiene que pedirlas explícitamente, que es lo que hace Hatboo.

`config/models.example.json` lleva la RAM residente de los siete locales en los tres
escalones de la escalera (2048 / 4096 / 8192), medida con Ollama 0,35,1 sobre 8,45 GB
sin GPU. No hay ningún número interpolado. Un `num_ctx` **fuera** de esa escalera sí
queda sin número: el Governor lo descarta y lo dice, en vez de extrapolar el KV cache.

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

`--decisiones` es la única que no toca el modelo ni la RAM: corre solo el Engine
sobre la suite y saca precisión de nivel y de riesgo, las bandas de confianza y el
presupuesto que firma. Filtra por `--split dev|validacion|todos` (por defecto `dev`,
24 de las 30 entradas). Medida en su portátil: 1,4 ms de media por turno con la
máquina tranquila, 3,7 ms con una compilación de por medio, y **sale con código 0
aunque baje la
precisión**: es lectura, no umbral.

Lo que no se pudo comprobar se reporta como `sin_comprobar`, nunca como acierto.

## Estado

Compila y pasan las pruebas: **350 verdes** — 240 unitarias del crate, 109 en los 9
ficheros de integración de §10 (context 8 · decision 18 · golden 5 · planner 19 ·
prompt 9 · recovery 13 · resources 11 · security 10 · verification 16) y 1 doc-test.
`clippy --all-targets --all-features -D warnings` a cero y **sin un solo `#[allow]`**;
la misma tanda corre en GitHub (`windows-latest`, rama `main`).

La batería `--decisiones` mide el Engine sobre `benchmarks/base.json`: **24/24 en nivel
y 24/24 en riesgo** con `--split dev` (lo que corre la CI) y 6/6 con `--split
validacion`. Ese 100 % es saturación, no virtud: la suite se calibró contra el motor,
así que hoy sirve como detector de regresiones y no para descubrir cosas — para eso
hacen falta entradas nuevas. `--decisiones` **sale con código 0 aunque baje la
precisión**: en la CI es lectura, no umbral.

Sin construir: las fases 6 (logprobs) y 7 (backend de decisión tipo Laya), y no hay
modo de encenderlas por error — `Brain` rechaza la configuración nombrando el campo
del JSON y la fase que falta. La Fase 8 (auto-mejora) del Plan v1.4 tampoco está.

Pendiente de medir, no de escribir: la comparación de §9 contra la línea base real
necesita correr el arnés con un proveedor cargado. Las cifras de RAM de este repo son
de una máquina (8,45 GB sin GPU, Ollama 0,35,1); en otra hay que volver a sondear.

## Pruebas

```bash
cargo test                    # todo
cargo test --lib              # el crate solo
cargo run --bin brain-bench -- --ayuda
```

## Licencia

MIT.
