"""An n-body integrator, written the way physics code in Python is written.

The interpreter workload is all dispatch and no arithmetic. This is the
opposite: a tight numeric loop over a handful of objects, where sealing buys
unboxed float slots rather than direct calls. Between the two of them the
answer to M0.4 is bracketed rather than balanced on one number.

The algorithm and the initial conditions are the standard n-body benchmark,
the Jovian planets plus the sun with a momentum offset applied so the system
does not drift. The usual Python version of it flattens everything into tuples
and lists because that is faster under CPython. This one uses a class with
seven attributes, because that is what somebody writing a simulation actually
does, and because an object with attributes is the thing sealing operates on.
"""

import math

SOLAR_MASS = 4 * math.pi * math.pi
DAYS_PER_YEAR = 365.24


class Body:
    def __init__(self, x, y, z, vx, vy, vz, mass):
        self.x = x
        self.y = y
        self.z = z
        self.vx = vx
        self.vy = vy
        self.vz = vz
        self.mass = mass


def bodies():
    return [
        Body(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, SOLAR_MASS),
        Body(
            4.84143144246472090e00,
            -1.16032004402742839e00,
            -1.03622044471123109e-01,
            1.66007664274403694e-03 * DAYS_PER_YEAR,
            7.69901118419740425e-03 * DAYS_PER_YEAR,
            -6.90460016972063023e-05 * DAYS_PER_YEAR,
            9.54791938424326609e-04 * SOLAR_MASS,
        ),
        Body(
            8.34336671824457987e00,
            4.12479856412430479e00,
            -4.03523417114321381e-01,
            -2.76742510726862411e-03 * DAYS_PER_YEAR,
            4.99852801234917238e-03 * DAYS_PER_YEAR,
            2.30417297573763929e-05 * DAYS_PER_YEAR,
            2.85885980666130812e-04 * SOLAR_MASS,
        ),
        Body(
            1.28943695621391310e01,
            -1.51111514016986312e01,
            -2.23307578892655734e-01,
            2.96460137564761618e-03 * DAYS_PER_YEAR,
            2.37847173959480950e-03 * DAYS_PER_YEAR,
            -2.96589568540237556e-05 * DAYS_PER_YEAR,
            4.36624404335156298e-05 * SOLAR_MASS,
        ),
        Body(
            1.53796971148509165e01,
            -2.59193146099879641e01,
            1.79258772950371181e-01,
            2.68067772490389322e-03 * DAYS_PER_YEAR,
            1.62824170038242295e-03 * DAYS_PER_YEAR,
            -9.51592254519715870e-05 * DAYS_PER_YEAR,
            5.15138902046611451e-05 * SOLAR_MASS,
        ),
    ]


def offset_momentum(system):
    px = 0.0
    py = 0.0
    pz = 0.0
    for body in system:
        px -= body.vx * body.mass
        py -= body.vy * body.mass
        pz -= body.vz * body.mass
    sun = system[0]
    sun.vx = px / SOLAR_MASS
    sun.vy = py / SOLAR_MASS
    sun.vz = pz / SOLAR_MASS


def advance(system, dt, steps):
    n = len(system)
    for _ in range(steps):
        for i in range(n):
            a = system[i]
            for j in range(i + 1, n):
                b = system[j]
                dx = a.x - b.x
                dy = a.y - b.y
                dz = a.z - b.z
                d2 = dx * dx + dy * dy + dz * dz
                mag = dt / (d2 * math.sqrt(d2))
                am = a.mass * mag
                bm = b.mass * mag
                a.vx -= dx * bm
                a.vy -= dy * bm
                a.vz -= dz * bm
                b.vx += dx * am
                b.vy += dy * am
                b.vz += dz * am
        for body in system:
            body.x += dt * body.vx
            body.y += dt * body.vy
            body.z += dt * body.vz


def energy(system):
    total = 0.0
    n = len(system)
    for i in range(n):
        a = system[i]
        total += 0.5 * a.mass * (a.vx * a.vx + a.vy * a.vy + a.vz * a.vz)
        for j in range(i + 1, n):
            b = system[j]
            dx = a.x - b.x
            dy = a.y - b.y
            dz = a.z - b.z
            total -= (a.mass * b.mass) / math.sqrt(dx * dx + dy * dy + dz * dz)
    return total


def run(steps):
    system = bodies()
    offset_momentum(system)
    before = energy(system)
    advance(system, 0.01, steps)
    return before, energy(system)


if __name__ == "__main__":
    import sys

    steps = int(sys.argv[1]) if len(sys.argv) > 1 else 500_000
    before, after = run(steps)
    print(f"{before:.9f}")
    print(f"{after:.9f}")
