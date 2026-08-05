# Thermal and Power Diagnosis

Use for heat, loud fans, throttling, short battery life, or unexpected power
drain.

## Initial evidence

```json
{"command":"run","args":["--domain","thermal","the laptop is overheating"]}
```

Inspect:

- `sensors`: thermal zones, hwmon temperatures, fans, AC, battery;
- `top-cpu`: sampled CPU consumers;
- `load`: sustained host pressure;
- `resources`: memory pressure that can increase CPU and I/O work.

## Decision tree

1. **90 °C or above**
   - Treat as critical and reduce workload.
2. **80–89 °C**
   - Check sustained CPU load, fan RPM, airflow, and power profile.
3. **High load with low fan RPM**
   - Suspect fan control, firmware, or hardware.
4. **Low battery**
   - Avoid long updates, model loads, or destructive operations.
5. **Normal temperature with poor performance**
   - Do not claim thermal throttling without frequency/throttle evidence.

The current sensor surface does not expose CPU-frequency throttling counters.
State this limitation when relevant.
