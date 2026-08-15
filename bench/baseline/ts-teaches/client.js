"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
var lib_1 = require("./lib");
var harborKm = 42;
var route = (0, lib_1.startRoute)("north gate", 7);
route = (0, lib_1.addStop)(route, "harbor", harborKm);
console.log("origin=" + route.origin);
console.log("stops=" + route.stops.length);
console.log("distance=" + (0, lib_1.totalDistance)(route, "km"));
