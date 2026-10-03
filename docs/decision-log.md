# Registro de decisiones

§1 del Plan v1.3.1: una decisión cerrada **solo** se reemplaza con una entrada
nueva aquí — número, razón, y a cuál sustituye si la sustituye. Las entradas no
se borran ni se reescriben: una prueba en `tests/golden.rs` comprueba que la
última entrada sobre el `Plan` dice el `SCHEMA_VERSION` que tiene el código, así
que cambiar el uno sin cambiar la otra deja la prueba en rojo.

| # | Fecha | Decide | Razón | Sustituye a |
|---|---|---|---|---|
| 1 | 2026-09-30 | Un solo `Plan` por turno, con escala N0–N3 | Canon §IX: las etiquetas de UI (Instantáneo/Normal/…) no son un campo de esfuerzo | — |
| 2 | 2026-09-30 | `level` administra y `thinking` razona: ejes distintos | Canon §II; `thinking` no sustituye a `level` ni es un `effort` del Brain | — |
| 3 | 2026-09-30 | Fast Path devuelve `DecisionResult` completo o `None` | Canon §VII: un resultado a medias dejaba `skip_generative` sin salida | — |
| 4 | 2026-10-02 | El Plan es **schema 7** | §11 del Plan pide `system + turno + historial + contexto + salida ≤ num_ctx`, y sin `mensaje_tokens`, `historial_turnos` e `historial_tokens` esa cuenta no se puede hacer con el firmante en la mano. El Canon y el Plan decían schema 6 con una lista donde esos campos no estaban | sustituye al schema 6 (Canon §VIII, Plan §4) |
| 5 | 2026-10-03 | El presupuesto se aprieta por el contexto, y la salida es lo último que se toca | Canon §XI: los tokens de salida son el 98 % del coste medido en Fase 0 | — |
| 6 | 2026-10-03 | Una salida cortada por el techo es `Truncado`, y **no** se reintenta igual | El techo vive en el Plan firmado (§VIII: inmutable); reintentar reproduce el corte y cobra dos llamadas al modelo | sustituye a «todo fallo de contrato es `Formato`» |
| 7 | 2026-10-03 | `structured_output` solo vale si lo dice una sonda | Medido el 03-10 en el equipo de él: Ollama no lo declara en `capabilities` y un `format: json_schema` contestó HTTP 400 en `gemma3:1b` y en `qwen3:1.7b` | sustituye a «copiar de la ficha del proveedor» |
| 8 | 2026-10-03 | Lo sensible no sale del equipo: pieza marcada → descartada si el Plan va a API | Canon §XII (`sensitivity` por ítem) y su regla de nunca enrutar datos propios a otro proveedor | — |
| 9 | 2026-10-03 | La elección entre candidaturas es `argmax p(nivel) × peso(nivel, riesgo)`, con los pesos en `config/tuning.json` y **default neutral (todo a 1,0)** | §3 y §15.3 del Plan piden la fórmula; los números tienen que salir de la Fase 3 sobre la suite de §9. Con 1,0 la conducta es la medida hasta hoy (batería 30/30 y las mismas tres bandas: 17 / 4 / 9), así que la pieza existe sin inventarse un coste | sustituye a «gana la mayor puntuación» |
| 10 | 2026-10-03 | `--sondear` mide también la salida estructurada (pidiendo un esquema ceñido y comprobando el JSON con `mini_schema`) | Es el único productor honesto del campo: la ficha del servidor no lo declara | — |
| 11 | 2026-10-03 | BM25 para repartir el hueco del contexto, detrás de `bm25Contexto` **apagado** | §5.1 pide el ranking; `k1 = 1,2` y `b = 0,75` son los de la receta, no medidas en esta máquina. Con el flag encendido cambia qué pieza se queda fuera (hay una prueba que lo muestra), así que encenderlo sin medir la precisión en la suite de §9 sería cambiar conducta sin poder decir que mejoró | — |
| 12 | 2026-10-03 | Elegida la ruta (a) del choque 4: **el papel se mueve a schema 7**, el crate no se toca | Decidido por él el 03-10. Canon y Plan a v1.3.2: el struct del Canon y la lista del Plan §4 llevan ahora los cinco campos serializados que faltaban (`system_tokens`, `mensaje_tokens`, `historial_turnos`, `historial_tokens`, `risk`) y las tres menciones operativas dicen 7. Las dos menciones dentro del historial de v1.2 se dejan: son historia, no especificación. Extraído del código (`awk` sobre `plan.rs`), no de memoria | cierra la pendencia de la entrada 4 |
| 13 | 2026-10-03 | `Reglas::empotradas()` y `Herramientas::empotradas()`: el crate trae su propio `config/` dentro del binario | Al conectar Hatboo salió que `Reglas::default()` está **vacío** y que `Cargada::leer(None)` hace lo mismo: un producto empaquetado no tiene `brain-rules.json` en el disco, así que el Brain arrancaba sin candidatear nada y lo que se obtenía era un Plan de suelo sin decirlo. Los JSON del repo entran por `include_str!`; `Cargada::leer(dir)` sigue mandando cuando el producto sí tiene directorio, y un JSON que no parsea **devuelve error en vez de vacío** | sustituye a «las reglas viven solo en un directorio de config que pone el producto» |
