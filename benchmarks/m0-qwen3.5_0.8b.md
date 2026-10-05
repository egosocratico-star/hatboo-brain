# Baseline vs Brain (§9)

- ficheros: `benchmarks/volcados/m0-qwen3.5_0.8b-baseline.json` (modo baseline, 3 reps, split dev) · `benchmarks/volcados/m0-qwen3.5_0.8b-brain.json` (modo brain, 3 reps, split dev)
- brain_version: `0.1.0` vs `0.1.0`
- umbral de regresión 5 % sobre medianas; con menos de 3 repeticiones **no se afirma nada** (ruido medido por corrida: +34 % / −11 %).
- **solo se comparan las entradas que generaron en los dos bandos**: una entrada que el Governor rechazó no es una respuesta rápida, y mezclarlas da «-99,9 % de duración» sin haber contestado. Lo que se queda fuera va escrito abajo.

## qwen3.5:0.8b

```text
  generaron             24/24 → 23/24 (sin generar 1)
Comparado sobre 23 entradas (las que generaron en los dos bandos). Fuera: 1 generadas solo en baseline, 0 solo en brain.
  duración ms           84237 →        17658      -79.0 %   mejora
  ttft ms                   0 →            0            —   sin base
  tokens salida          1024 →          192      -81.2 %   mejora
  rango dur ms   [27335–109220] → [12871–118195]
  rango toks       [291–1024] →    [78–1024]
  recargas tot              0 →            6            —   sin base  (gasto nuevo del brain, no evitado)
  reintentos tot            0 →            4            —   sin base  (gasto nuevo del brain, no evitado)
  checks ✓/✗/—     0 / 0 / 72 →  0 / 27 / 45
  Verificado                0 →            0
```

**Veredicto qwen3.5:0.8b**: mejora — 2 métricas por debajo del umbral y 0 por encima, sobre 23 entradas comparables.

**Veredicto**: mejora en 2 métricas de coste por respuesta, sin ninguna por encima del umbral, sobre 3 repeticiones compartidas.
