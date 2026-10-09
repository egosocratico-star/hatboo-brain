# Change log de `hatboo-brain`

Historial para **quien consume el crate**. El orden es el de `git log` (31 commits
desde el 30-09), agrupado por lo que se nota desde fuera: qué contratos cambian, qué
se arregla y qué instrumento hay para medirlo. Las decisiones con su razón y su fecha
viven en [`docs/decision-log.md`](docs/decision-log.md); el Threat model y lo que
queda abierto, en [`docs/threat-model.md`](docs/threat-model.md).

**Sin etiquetas todavía.** `Cargo.toml` dice `0.1.0` y el repo no tiene ningún `v*`:
la versión se publica cuando se corre la primera etiqueta, y eso es decisión de quien
lo consume. No hay `SemVer` que romper: los cinco contratos son inestables por
declaración hasta que exista un segundo consumidor.

---

## Sin etiquetar — 0.1.0 en curso

### Lo que puede pedirle un producto

- **Cinco contratos estables de nombre**: `Request` (`api/request.rs`) ·
  `Decision` (`decision/`) · `Plan` (`planner/plan.rs`, **schema 7**) ·
  `Provider` (`providers/`) · `Verification` (`verification/`). Un producto monta un
  `Brain` con su `Registry`, su sonda de recursos y sus proveedores
  (`brain::Montaje`), y llama a `run` / `run_with`.
- **`contexto_producto`** (05-10): el material que el producto añade a cada turno y
  no es conversación —plantillas, memoria, notas del modo— entra por su propio campo
  y se convierte en una capa del *system* que **no se recorta nunca**. Antes no había
  dónde ponerlo y los consumidores lo cabalgaban en la cabecera del historial, que es
  lo primero que corta el presupuesto.
- **`respetar_modelo`** (05-10, entrada 22): con `true`, si el modelo que eligió el
  producto no puede correr **no se firma otro**; la corrida falla con
  `BrainError::ModeloPedidoNoCorre` llevando dentro el motivo del Governor y sus
  tres cifras. Apagado por defecto. La frontera quedó cerrada el 05-10 (entrada 23):
  el modelo lo elige el producto; el Brain escala de tier solo **después** de un
  fallo real, y entonces deja evento, linaje (`parent_plan_hash`) y motivo.
- **El Governor ya no niega un turno por RAM** (06-10, entrada 26): la escalera de
  `num_ctx` baja a cinco peldaños {512, 1024, 2048, 4096, 8192}, cada nivel distingue
  lo que **pide** (`num_ctx_minimo`) de lo que **aún firma** (`num_ctx_suelo`), y
  `Consejo.ajuste` dice qué se cedió: `Ninguno`, `VentanaCorta` o `SinMargen`. Con el
  modelo residente a una ventana que cumple el nivel no se le mueve (histéresis), que
  es lo que producía las recargas de 3,7 s. Solo se niega algo cuando no hay datos:
  sonda ciega, o modelo sin una cifra medida.
- **`keep_alive` lo decide el módulo de recursos** (06-10, entrada 27):
  `resources::mantener(ajuste, presion)` — 900 s si cabía con margen, 300 s si hubo
  que bajar de peldaño, 60 s si hubo que gastar el margen, y nunca más de 60 s bajo
  presión de batería o CPU. El Plan firma ese valor; `Plan.keep_alive` ya existía, así
  que el contrato no cambia de número.
- **`logprobs` (Fase 6, 04-10)**: viaja en el `GenerationRequest`, se cose durante el
  stream y sale en `TaskMetrics` (`logprob_medio` y `probabilidad`). **No decide
  nada**: §1 deja el origen estadístico en solo registro hasta que exista la
  calibración medida. En Anthropic no hay log-probabilidades: el campo queda `None` y
  se dice.
- **Las reglas y el catálogo de tools viajan dentro del binario** (03-10, entrada 13):
  `Reglas::empotradas()` y `Herramientas::empotradas()` leen por `include_str!` los
  JSON de `config/`, y un JSON que no parsea **devuelve error en vez de vacío**. Un
  producto empaquetado ya no arranca con el Engine mudo.
- **El muestreo lo manda la config** (04-10, entrada 18): `temperatura` y `semilla`
  pasan a `BrainConfig` con default `None` = no mandar la clave. Antes estaban
  quemados en el runtime (0,0 y 42) y se aplicaban a todas las conversaciones.
- **`default = ["ollama"]`** (04-10, entrada 20): las otras tres puertas de API
  (`openai`, `anthropic`, `generic`) se encienden explícitamente.
- **El hash de confianza de `HATBOO.md` es SHA-256** (04-10, entrada 21); los de
  linaje (`plan_hash`, `parent_plan_hash`, la caché) siguen en FNV-1a-64 a propósito,
  porque no deciden permisos. Cuesta la dependencia `sha2`, y por eso está escrita.

### Arreglos que cambian conducta

- **El registro de Ollama se arma con modelos, no con filas de `/api/tags`** (08-10).
  El servidor repite un nombre por cada digest que contesta a ese tag y añade un alias
  `llamacpp:<hash>` por cada gguf importado: medido en este equipo, **14 filas para 10
  modelos**. `bases_de_tags` deja fuera el alias cuando su digest tiene nombre propio, y
  deja una sola fila por tag (la primera, que es el orden del servidor). No era
  cosmética: el `decisiones.jsonl` de Hatboo guarda un turno del 07-10 firmado con
  `llamacpp:83be9dbf…`, un id que no está en la tabla medida —el Governor se queda sin
  cifras y la histéresis del residente no lo reconoce aunque `gemma3:1b` esté cargado—.
  Efecto lateral: cada fila que sobra era un `/api/show` en cada turno.

- **Un turno de charla no oye hablar de herramientas** (08-10, entrada 31). Se le
  echó la culpa al modelo de divagar y era el prompt: en un saludo sin herramientas el
  system llevaba una línea negando las aprobaciones, la capa de herramientas decía «Sin
  herramientas en este turno», el contrato hablaba de archivos y el rol por defecto de
  `Identidad` decía «ejecutas lo pactado». Un 0,8 B repite lo que lee, y lo repitió
  literalmente. Ahora esas cuatro capas no nombran ninguna capacidad cuando el Plan no
  trae herramientas, las etiquetas de intent son etiquetas («Turno: una pregunta.») y
  hay una prueba de propiedad que lo vigila. **Rompido a propósito**: cambia el texto
  de `Identidad::default().rol` y el de las dos ramas del contrato, así que un producto
  que compare hashes de prefijo entre versiones verá el salto.
- **El techo de razonamiento del producto llega al Plan** (08-10, entrada 31). Estaba
  roto de otra manera: el runtime resolvía el techo en `PlanContext.ceiling`
  (`req.thinking_ceiling.or(config.thinking_ceiling)`) y el Armador leía el campo del
  pedido en crudo —los productos ponen el nivel en la **config**—, así que el plan
  firmaba `ThinkingLevel::Off` en **todos** los niveles. Medido con el `MockProvider`
  devolviendo el system ya ensamblado: config `Medium` → plan `Off`. Ahora el Armador lee
  `e.ctx.ceiling`, y la regla es pública: N0 no piensa (es el nivel de contestar ya), N1
  sube como mucho a `Bajo`, del N2 arriba se honra el techo pedido. Cuando el nivel lo
  apaga, el `reason` del Plan lo escribe
  («· razonamiento medio pedido y apagado por el nivel Instantáneo») para que el producto
  pueda explicarlo en vez de parecer tonto.

- **`plan.timeout_s` deja de ser una fecha de vencimiento para el turno entero**
  (07-10, entrada 30). El stream se corta cuando pasan `timeout_s` **sin una sola
  señal**, o cuando se supera lo que el propio Plan autorizó generar a una
  decodificación conservadora (`techo_total`: respuesta + razonamiento a 5 tok/s, con
  el suelo de `timeout_s + 30` de siempre). Es el arreglo de una regresión que metió
  este crate el mismo día: al volverse real la lectura incremental, el tope de 30 s del
  N0 empezó a gobernar la generación entera, y un turno con el razonamiento encendido
  no cabe ahí —medido: 79,7 s hasta la primera letra de un «hola» con `Medio`, con
  1213 deltas de pensamiento por delante—. Antes de ese arreglo el tope no regía nada,
  porque el cuerpo se leía entero antes de arrancar el reloj. **Un producto que lea
  `Timeout` como «el modelo no sirve» va a leer mal este caso**: ahora `Timeout`
  significa silencio de verdad.

- **`Off` de razonamiento se le dice al servidor, no se calla** (07-10, entrada 29).
  `cuerpo_de` manda `think` **siempre**: `false` cuando el nivel es `Off`. Mandarlo solo
  con `true` dejaba que Ollama decidiera, y los modelos que nacen pensando deciden
  pensar: medido con `qwen3.5:0.8b`, un pedido de chat se fue a 17,5 s con
  `done_reason: "length"`, `eval_count: 160` y **respuesta visible vacía** —se gastó el
  tope entero en pensamiento—. Con `think: false`, el mismo pedido contesta en 3090 ms.
  Probado además sobre `gemma3:1b` y `deepseek-r1:1.5b`: aceptan el booleano y responden
  igual, así que enviarlo no es una apuesta.
- **El stream lee el razonamiento donde el Ollama de verdad lo pone** (07-10, entrada
  29). Este servidor manda el pensamiento en `message.thinking`; `juntar` ya leía las
  dos claves, pero el emisor de deltas leía solo `reasoning`, así que un turno que piensa
  no mandaba **ninguna** letra a la interfaz hasta el final. Ahora el delta de
  razonamiento sale de `reasoning` **o** `thinking`. Medido tras el arreglo, con el
  pensamiento otra vez en manos del usuario: 26 deltas, el primero a **197 ms**, el último
  a 2565 ms, hueco medio 94 ms, 15,7 tok/s.

- **El stream de Ollama suelta cada letra en cuanto llega** (07-10, entrada 28).
  `providers/ollama.rs` leía el cuerpo NDJSON entero (`leer_ndjson`) y lo reenviaba ya
  terminado con `stream::iter`, así que un `Emitidor` del producto no veía un solo
  `Token` hasta que el modelo acababa: en el chat la respuesta aparecía **de golpe**,
  que es lo que él llamó «la generación se ve fea». Ahora `Transmision` corta por
  `\n` y emite cada línea al cerrarse; las cuentas del `Final` siguen saliendo de
  `juntar` con **todas** las líneas, así que ningún número se pierde. La prueba nueva
  (`cada_letra_sale_antes_de_que_termine_el_cuerpo`) usa un servidor a mano que **no
  escribe la segunda línea hasta que el cliente pide la primera**: si alguien vuelve a
  acumular el cuerpo, el test no pasa a medias — se cuelga y falla por `timeout`.
- **`ttft_ms` es tiempo hasta el primer token, no duración de la generación**
  (07-10, entrada 28). El runtime lo escribía con `inicio.elapsed()` **después** de
  cerrar el bucle del stream, o sea la corrida entera disfrazada de primer token. Se
  toma en el primer delta (de texto o de razonamiento: salen del mismo decodificador),
  y un turno sin letras deja `None` en vez de una cifra inventada. **Los `ttft ms` de
  los informes anteriores a esta entrada están medidos con la definición vieja**: no
  son comparables con los de después, y donde diga «ttft» antes del 07-10 hay que leer
  «duración».
- **El prompt del turno se adapta a lo que el Plan firmó** (07-10, entrada 28). Tres
  cosas que estaban fijas para todo:
  - la capa `contrato` tiene dos textos: las cinco líneas de *plan firmado / tools
    listadas / archivos* solo entran cuando el Plan trae herramientas; sin ellas dice
    que no se ejecuta nada, que es lo que de verdad aplica;
  - `piezas_del_pedido` y `piezas_de_seguridad` reciben `puede_actuar`
    (= `!plan.tools.is_empty()`): los hechos del proyecto (raíz, tests, lint, build) y
    la cuenta de aprobaciones y escrituras permitidas no se meten en un chat;
  - la capa `modo` lleva una línea nombrando el intent (`instruccion_del_intent`), que
    es el hueco que `instrucciones_modo: None` dejaba vacío desde el principio.

  La puerta anti-inyección no se toca: el `<datos origen=…>` se queda en las dos ramas
  del contrato, y una tool fuera del Plan la sigue rechazando el código.

- **El benchmark ya no puede vender un rechazo como velocidad** (`ec693b3`, entrada
  24): `--comparar` solo enfrenta las entradas que generaron en los dos bandos, las
  medianas de coste se calculan sobre lo que generó, los contadores de eventos van en
  suma y ningún coste sale sin su rango.
- La invariante de §11 cuenta el turno y el historial, y el historial deja de mandarse
  dos veces (`f30a59c`).
- El razonamiento reservado por el Plan es el que recibe el proveedor (`b1f1a3c`); una
  tool rechazada se le dice al modelo y la métrica cuenta el fallo (`79c194d`);
  degradado al revés, y cuando tampoco cabe el plan seguro se dice (`beee8e1`); escalar
  de tier no regala tools (`d944f71`); un solo margen y un solo tope por petición a
  OpenAI (`a3fc69a`); el estado de la tarea es real y la traza dice lo que sobró
  (`7b96868`).
- Los siete flags apagan algo de verdad (`43ed6ee`) y la auditoría de aprobaciones,
  clases de fallo y tope de salida (`4732c96`).
- `structured_output` solo vale si lo dice una sonda (03-10, entrada 7): la ficha de
  Ollama no lo declara y un `format: json_schema` contestó HTTP 400.

### Instrumento

- `brain-bench --decisiones`: el Engine sobre la suite de §9 sin tocar modelo ni RAM.
  Reporta precisión de nivel y riesgo, las tres bandas de confianza, y desde el 06-10
  **cuántas reglas casan por entrada y el histograma de valores de confianza** —el
  paso 1 del gate de G.1, que ya dio su número: de los 14 «alto», doce son 1,000
  porque no hubo competencia.
- `--sondear` mide RAM por `num_ctx` y prueba tool call, think y salida estructurada
  reales (03-10, entrada 10). `--comparar A B` y `--informe` sacan el veredicto del
  gate (§9) con sus reglas. `--salida` vuelca los crudos a `benchmarks/volcados/`.

### Puertas

CI en `windows-latest` con `clippy --all-targets --all-features -D warnings`, pruebas
y la batería de decisiones (`e757c6d`). `Cargo.lock` viaja en el repo (04-10, entrada
16) para que la CI no resuelva dependencias al último índice.
