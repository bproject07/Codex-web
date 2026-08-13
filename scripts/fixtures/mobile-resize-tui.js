"use strict";

const HISTORY_LINES = 8_000;
let redrawCount = 0;
let redrawTimer = null;

function redraw() {
  redrawCount += 1;
  const columns = process.stdout.columns ?? 80;
  const rows = process.stdout.rows ?? 24;
  const output = ["\u001b[3J\u001b[2J\u001b[H"];

  for (let index = 0; index < HISTORY_LINES; index += 1) {
    output.push(
      `fixture history ${String(index).padStart(4, "0")} ` +
        `r=${redrawCount} ${columns}x${rows}\r\n`,
    );
  }
  output.push(`fixture prompt redraw=${redrawCount} size=${columns}x${rows}> `);
  process.stdout.write(output.join(""));
}

function scheduleRedraw() {
  if (redrawTimer !== null) {
    clearTimeout(redrawTimer);
  }
  redrawTimer = setTimeout(() => {
    redrawTimer = null;
    redraw();
  }, 20);
}

process.stdout.on("resize", scheduleRedraw);
process.stdin.setRawMode?.(true);
process.stdin.resume();
process.stdin.on("data", (data) => {
  if (data.includes(3)) {
    process.exit(0);
  }
  process.stdout.write(data);
});

redraw();
