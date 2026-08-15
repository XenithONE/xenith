import { startRoute, addStop, totalDistance } from "./lib";

const harborKm = 42;
let route = startRoute("north gate", 7);
route = addStop(route, "harbor", harborKm);
console.log("origin=" + route.origin);
console.log("stops=" + route.stops.length);
console.log("distance=" + totalDistance(route, false));
