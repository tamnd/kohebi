"""A tree-walking interpreter for a small imperative language.

M0.4 needs a workload dominated by the two things sealing removes: attribute
access on user-defined classes, and polymorphic method dispatch. An AST
interpreter is the purest real example of both. Every step of the inner loop is
`node.eval(env)` on a node whose class is not known statically, followed by
`self.left` and `self.right` on an object whose layout is not known statically.

It is also the shape of a great deal of real Python. Template engines, query
builders, rules engines, config languages and serializers are all tree walkers
written exactly like this, and they are all slow for exactly this reason.

The Python here is written the way someone would write it without knowing that
a Rust port was coming: a class per node type, an `eval` method on each, an
`Env` object with a dict and a parent pointer. No `__slots__`, no dispatch
table, no attempt to be clever. Making it faster in Python would make the
experiment measure the optimisation rather than the runtime.
"""


class Env:
    def __init__(self, parent=None):
        self.vars = {}
        self.parent = parent

    def get(self, name):
        env = self
        while env is not None:
            if name in env.vars:
                return env.vars[name]
            env = env.parent
        raise NameError(name)

    def set(self, name, value):
        self.vars[name] = value


class Return(Exception):
    def __init__(self, value):
        self.value = value


class Num:
    def __init__(self, value):
        self.value = value

    def eval(self, env):
        return self.value


class Var:
    def __init__(self, name):
        self.name = name

    def eval(self, env):
        return env.get(self.name)


class BinOp:
    def __init__(self, op, left, right):
        self.op = op
        self.left = left
        self.right = right

    def eval(self, env):
        a = self.left.eval(env)
        b = self.right.eval(env)
        op = self.op
        if op == "+":
            return a + b
        if op == "-":
            return a - b
        if op == "*":
            return a * b
        if op == "//":
            return a // b
        if op == "%":
            return a % b
        raise ValueError(op)


class Compare:
    def __init__(self, op, left, right):
        self.op = op
        self.left = left
        self.right = right

    def eval(self, env):
        a = self.left.eval(env)
        b = self.right.eval(env)
        op = self.op
        if op == "<":
            return a < b
        if op == ">":
            return a > b
        if op == "==":
            return a == b
        if op == "<=":
            return a <= b
        raise ValueError(op)


class Assign:
    def __init__(self, name, expr):
        self.name = name
        self.expr = expr

    def eval(self, env):
        value = self.expr.eval(env)
        env.set(self.name, value)
        return value


class If:
    def __init__(self, test, then, orelse):
        self.test = test
        self.then = then
        self.orelse = orelse

    def eval(self, env):
        if self.test.eval(env):
            return self.then.eval(env)
        if self.orelse is not None:
            return self.orelse.eval(env)
        return 0


class While:
    def __init__(self, test, body):
        self.test = test
        self.body = body

    def eval(self, env):
        result = 0
        while self.test.eval(env):
            result = self.body.eval(env)
        return result


class Block:
    def __init__(self, stmts):
        self.stmts = stmts

    def eval(self, env):
        result = 0
        for stmt in self.stmts:
            result = stmt.eval(env)
        return result


class Func:
    def __init__(self, name, params, body):
        self.name = name
        self.params = params
        self.body = body

    def eval(self, env):
        env.set(self.name, self)
        return 0

    def call(self, args, env):
        local = Env(env)
        params = self.params
        for i in range(len(params)):
            local.set(params[i], args[i])
        try:
            self.body.eval(local)
        except Return as r:
            return r.value
        return 0


class Call:
    def __init__(self, name, args):
        self.name = name
        self.args = args

    def eval(self, env):
        func = env.get(self.name)
        args = []
        for arg in self.args:
            args.append(arg.eval(env))
        return func.call(args, env)


class Ret:
    def __init__(self, expr):
        self.expr = expr

    def eval(self, env):
        raise Return(self.expr.eval(env))


def program():
    """fib(n) recursively, then a loop that sums fib over a range.

    Recursion exercises call dispatch, environment chaining and the exception
    path used for `return`. The loop exercises the flat statement path. Between
    them they cover what a tree walker spends its time on.
    """
    fib = Func(
        "fib",
        ["n"],
        Block(
            [
                If(
                    Compare("<", Var("n"), Num(2)),
                    Ret(Var("n")),
                    Ret(
                        BinOp(
                            "+",
                            Call("fib", [BinOp("-", Var("n"), Num(1))]),
                            Call("fib", [BinOp("-", Var("n"), Num(2))]),
                        )
                    ),
                )
            ]
        ),
    )
    main = Block(
        [
            fib,
            Assign("total", Num(0)),
            Assign("i", Num(0)),
            While(
                Compare("<", Var("i"), Num(28)),
                Block(
                    [
                        Assign(
                            "total",
                            BinOp("+", Var("total"), Call("fib", [Var("i")])),
                        ),
                        Assign("i", BinOp("+", Var("i"), Num(1))),
                    ]
                ),
            ),
            Var("total"),
        ]
    )
    return main


def run(iterations):
    tree = program()
    result = 0
    for _ in range(iterations):
        result = tree.eval(Env())
    return result


if __name__ == "__main__":
    import sys

    n = int(sys.argv[1]) if len(sys.argv) > 1 else 1
    print(run(n))
