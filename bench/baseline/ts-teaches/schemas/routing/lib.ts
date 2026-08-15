export interface Stop {
    label: string;
    km: number;
}

export interface Route {
    stops: Stop[];
    origin: string;
}

export function startRoute(origin: string, firstKm: number): Route {
    return { stops: [{ label: "depot", km: firstKm }], origin };
}

export function addStop(route: Route, label: string, km: number): Route {
    return { ...route, stops: [...route.stops, { label, km }] };
}

export function totalDistance(route: Route, roundUp: boolean): number {
    const total = route.stops.reduce((sum, s) => sum + s.km, 0);
    return roundUp ? Math.ceil(total) : total;
}
