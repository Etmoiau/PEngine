@echo off
setlocal
cd /d "%~dp0"

cargo run --release
if errorlevel 1 (
    echo.
    echo O emulador encerrou com erro. Confira a mensagem acima.
    pause
)

endlocal
