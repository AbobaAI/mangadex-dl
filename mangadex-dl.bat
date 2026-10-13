@echo off
setlocal
chcp 65001 >nul
set PYTHONIOENCODING=utf-8
set PYTHONUTF8=1
where py >nul 2>&1
if %ERRORLEVEL%==0 (
  py -3 "%~dp0mangadex_dl.py" %*
  exit /b %ERRORLEVEL%
)
python "%~dp0mangadex_dl.py" %*
exit /b %ERRORLEVEL%
