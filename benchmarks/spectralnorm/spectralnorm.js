"use strict";
const N = 2500;

function evalA(i, j) {
  const ij = i + j;
  return 1.0 / (((ij * (ij + 1)) / 2 | 0) + i + 1);
}

function multiplyAv(v, av) {
  for (let i = 0; i < N; i++) {
    let sum = 0.0;
    for (let j = 0; j < N; j++) sum += evalA(i, j) * v[j];
    av[i] = sum;
  }
}

function multiplyAtv(v, atv) {
  for (let i = 0; i < N; i++) {
    let sum = 0.0;
    for (let j = 0; j < N; j++) sum += evalA(j, i) * v[j];
    atv[i] = sum;
  }
}

function multiplyAtAv(v, atav, tmp) {
  multiplyAv(v, tmp);
  multiplyAtv(tmp, atav);
}

const u = new Float64Array(N);
const v = new Float64Array(N);
const tmp = new Float64Array(N);
for (let k = 0; k < N; k++) u[k] = 1.0;

for (let iter = 0; iter < 10; iter++) {
  multiplyAtAv(u, v, tmp);
  multiplyAtAv(v, u, tmp);
}

let vBv = 0.0, vv = 0.0;
for (let i = 0; i < N; i++) {
  vBv += u[i] * v[i];
  vv += v[i] * v[i];
}
console.log(Math.sqrt(vBv / vv).toFixed(9));
