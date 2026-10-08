# Выпуск релиза / Releasing

Краткая памятка для владельца репозитория. Бинарные файлы (exe, DLL, zip) в репозиторий **не коммитятся**: у GitHub лимит 100 МБ на файл, а `cublasLt64_13.dll` один весит 470 МБ. Готовые сборки выкладываются как файлы релиза (Release assets, до 2 ГБ каждый).

## 1. Проверить

```powershell
. .\env.ps1
cargo fmt --check
cargo clippy --release --all-targets -- -D warnings                          # NVIDIA-конфигурация
cargo test --release
cargo test --release --no-default-features                                    # CPU-конфигурация (в отдельном CARGO_TARGET_DIR)
```

## 2. Версия

Поднять `version` в `Cargo.toml` (и `assemblyIdentity` в `assets/app.manifest`, если важно), закоммитить.

## 3. Собрать и упаковать

```powershell
.\scripts\build-release.ps1 -Variant nvidia     # exe без консоли, CUDA, RTX 40
.\scripts\build-release.ps1 -Variant cpu        # exe без консоли, без видеокарты
.\scripts\package.ps1                           # dist\release\*.zip и SHA256SUMS.txt
```

Перед загрузкой: закрыть запущенный VoicePaste (иначе упаковка не сможет перезаписать файлы) и проверить, что оба exe стартуют на чистом PATH (без CUDA Toolkit).

## 4. Тег и релиз

```powershell
git tag -a v1.0.0 -m "VoicePaste 1.0.0"
git push origin v1.0.0
gh release create v1.0.0 dist\release\VoicePaste-1.0.0-nvidia.zip dist\release\VoicePaste-1.0.0-cpu.zip dist\release\SHA256SUMS.txt `
  --title "VoicePaste 1.0.0" --notes-file docs\release-notes.md
```

В описании релиза стоит указать: для каких видеокарт NVIDIA-сборка, что CPU-сборке нужен процессор с AVX2, что модель скачивается из настроек, и контрольные суммы.

## Что не должно попасть в репозиторий

- `debug/` (записи голоса из `--dump-audio`), `models/`, `dist/`, `target*/` — уже в `.gitignore`.
- `.claude/settings.local.json` — локальные настройки инструмента с путями вашей машины, в `.gitignore`.
- Любые `*.bin`, `*.gguf`, `*.zip`, `*.dll`, `*.exe`.
