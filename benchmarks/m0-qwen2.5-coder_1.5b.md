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
