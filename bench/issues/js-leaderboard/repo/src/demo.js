"use strict";

const { topPlayers } = require("./leaderboard");
const { formatRow } = require("./format");

const players = [
  { name: "Ada Lovelace", points: 120 },
  { name: "Grace Hopper", points: 200 },
  { name: "Alan Turing", points: 50 },
];

for (const player of topPlayers(players, 3)) {
  console.log(formatRow(player));
}
