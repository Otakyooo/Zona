# Тестовые фикстуры

- `etc1s.ktx2`, `uastc.ktx2` — Basis Universal текстуры 40×40 (6 уровней) из
  репозитория крейта [`basisu`](https://github.com/marcogomez/basisu)
  (`basisu/tests/fixtures/`, Apache-2.0). Используются в тестах декодера
  KTX2/BasisU (`crates/zn-assets/src/images.rs`).
  - `etc1s.ktx2` — ETC1S c BasisLZ-суперкомпрессией (scheme 1);
  - `uastc.ktx2` — UASTC без суперкомпрессии (format UNDEFINED, scheme 0).
