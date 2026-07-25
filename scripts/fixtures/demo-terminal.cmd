@echo off
if /I "%~1"=="--version" (
  echo codex-web-community-demo 1.0.0
  exit /b 0
)

node "%~dp0demo-terminal.js"
