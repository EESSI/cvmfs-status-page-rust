# Startup overrides

Put customized HTML templates in `templates/` and public files in `resources/`.
Files overlay the embedded defaults. Restart the service to apply changes.
Only files under `resources/` are registered as public artifacts. Symlinks are
rejected. Do not place credentials in this directory.
