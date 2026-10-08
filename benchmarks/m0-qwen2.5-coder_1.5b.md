# Baseline vs Brain (§9)

- ficheros: `benchmarks/volcados/m0-qwen2.5-coder_1.5b-baseline.json` (modo baseline, 3 reps, split dev) · `benchmarks/volcados/m0-qwen2.5-coder_1.5b-brain.json` (modo brain, 3 reps, split dev)
- brain_version: `0.1.0` vs `0.1.0`
- umbral de regresión 5 % sobre medianas; con menos de 3 repeticiones **no se afirma nada** (ruido medido por corrida: +34 % / −11 %).
- **solo se comparan las entradas que generaron en los dos bandos**: una entrada que el Governor rechazó no es una respuesta rápida, y mezclarlas da «-99,9 % de duración» sin haber contestado. Lo que se queda fuera va escrito abajo.

## qwen2.5-coder:1.5b

```text
  generaron             24/24 → 23/24 (sin generar 1)
Comparado sobre 23 entradas (las que generaron en los dos bandos). Fuera: 1 generadas solo en baseline, 0 solo en brain.
  duración ms            3033 →        13480     +344.4 %   REGRESIÓN
  ttft ms                   0 →            0            —   sin base
  tokens salida            14 →           31     +121.4 %   REGRESIÓN
  rango dur ms   [1109–33486] → [1589–47857]
  rango toks          [3–415] →      [9–398]
  recargas tot              0 →           24            —   sin base  (gasto nuevo del brain, no evitado)
  reintentos tot            0 →           16            —   sin base  (gasto nuevo del brain, no evitado)
  checks ✓/✗/—     0 / 0 / 72 →  0 / 33 / 39
  Verificado                0 →            9
```

**Veredicto qwen2.5-coder:1.5b**: REGRESIÓN — 0 métricas por debajo del umbral y 2 por encima, sobre 23 entradas comparables.

**Veredicto**: REGRESIÓN en 2 métricas de coste por respuesta (0 por debajo del umbral), sobre 3 repeticiones compartidas. Ver arriba el veredicto de cada modelo: los dos no dicen lo mismo.

## Después del gate (05-10): qué hizo la recta abierta aquí

No forma parte del gate M0 y **no se compara con él**: son otras condiciones de RAM y
otra construcción. Queda escrito porque este informe cerró con dos gastos que el
mismo día se atacaron —24 recargas y 32 rechazos por falta de RAM convertidos en
«-99,9 % de duración»—, y alguien va a preguntar si se arreglaron.

Decisión 26 del `docs/decision-log.md`: la RAM dejó de ser una puerta. La escalera
bajó a cinco peldaños con los dos nuevos medidos (`hatboo/benchmarks/resultados/rama-2026-10-06.json`), y el Governor aprieta la ventana o gasta el margen en vez de
negar el turno. La escalera entera, con histéresis: con el modelo residente a una
ventana que cumple el nivel no se le mueve, que es lo que producía las recargas.

Pase corto sobre este mismo modelo, `--modo brain --reps 2 --categoria saludo
--margen 250` (volcado en `benchmarks/volcados/post-coder-saludo.json`):

```text
  RAM libre medida        1936 MB
  corridas                8 · las 8 generaron (0 rechazos)
  latencia total          1237 ms  [776–1704]
  tokens de salida        17  [11–24]
  RAM residente           1109 MB en las 8
  reintentos                  0
  recargas                    1   (la carga fría; las otras 7 fueron residentes)
  precisión de nivel       8/8
```

Lo que se puede decir con esto: **ocho turnos de saludo seguidos, una sola carga**.
Lo que no: no es una comparación con las 72 corridas de arriba —otro RAM libre, otra
categoría y dos repeticiones en vez de tres—, así que aquí no hay porcentajes. El
mismo `--comparar` con dos volcados al mismo nivel dirá el delta cuando se corra.
