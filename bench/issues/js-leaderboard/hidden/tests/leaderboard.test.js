"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const { topPlayers } = require("../../src/leaderboard");

const players = [
  { name: "Ada Lovelace", points: 120 },
  { name: "Grace Hopper", points: 200 },
  { name: "Alan Turing", points: 50 },
  { name: "Edsger Dijkstra", points: 75 },
];

test("top players come back best first", () => {
  const top = topPlayers(players, 4);
  assert.deepEqual(
    top.map((p) => p.name),
    ["Grace Hopper", "Ada Lovelace", "Edsger Dijkstra", "Alan Turing"]
  );
});

test("ranks start at 1 and follow the sort order", () => {
  const top = topPlayers(players, 3);
  assert.deepEqual(
    top.map((p) => [p.rank, p.points]),
    [[1, 200], [2, 120], [3, 75]]
  );
});

test("limit caps the result", () => {
  assert.equal(topPlayers(players, 2).length, 2);
  assert.equal(topPlayers(players, 2)[0].points, 200);
});

test("input array is not mutated", () => {
  const before = players.map((p) => p.name);
  topPlayers(players, 4);
  assert.deepEqual(players.map((p) => p.name), before);
});

test("numeric order, not string order", () => {
  const top = topPlayers([
    { name: "a", points: 9 },
    { name: "b", points: 100 },
    { name: "c", points: 30 },
  ]);
  assert.deepEqual(
    top.map((p) => p.points),
    [100, 30, 9]
  );
});
