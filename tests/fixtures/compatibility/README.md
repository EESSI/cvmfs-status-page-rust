# Frozen generator compatibility fixtures

These outputs were generated from pre-extraction commit
`a6dfd0ed365a5188f1458b446cd61bbf0c91d1ea` using its original binary, local HTTP
fixtures and existing file formats. They cover healthy, unavailable, catching-up,
expired-grace, history-disabled, Grafana-fallback, nested-path and custom-template
behavior. Assets are checked byte-for-byte between export and HTTP.

`tests/test_service.py` compares the retained generator and HTTP service against
these files. Normalization covers only generated footer/JSON/metric timestamps,
first-observation times, current history sample times (including uptime and revision
series), Grafana fetch time, the deliberately seeded historical sample time, and
calendar bucket dates expressed as offsets from the evaluation day. Source manifest
revision timestamps, live Grafana sample timestamps, values, counts, health,
markup, whitespace, escaping, URLs and public configuration remain unchanged.
Unordered response arrays and metric lines use the existing comparison normalizer.

To intentionally regenerate from an independently built reference:

```sh
python3 tests/test_service.py --write-fixtures /path/to/reference-generator
```

Do not refresh fixtures from the candidate binary to make a mismatch pass.
