@echo off
if /I "%~1"=="--version" (
  echo codex-mobile-resize-fixture 1.0.0
  exit /b 0
)

node "%~dp0mobile-resize-tui.js"
