#!/usr/bin/env python3
"""Writes the sample Python project the code self-test works in.

    sample.py <folder> <venv>

The folder is replaced. It gets a package of two modules, a test file with
one test that fails on purpose, a script with a `__main__` guard, a type
error and an unused import, a git repository, and `.venv` as a link to
`<venv>` (an environment with basedpyright, ruff, debugpy and pytest, which
is only read).
"""
import os
import shutil
import subprocess
import sys

FILES = {
    ".gitignore": ".venv\n__pycache__/\n.pytest_cache/\n",
    "shop/__init__.py": '"""A sample shop."""\n',
    "shop/models.py": '''"""The order model of the sample shop."""
from dataclasses import dataclass


@dataclass
class Order:
    item: str
    quantity: int
    unit_price: float

    def total(self) -> float:
        return self.quantity * self.unit_price

    def scaled(self, factor: int) -> "Order":
        return Order(self.item, self.quantity * factor, self.unit_price)

    def describe(self) -> str:
        return f"{self.quantity} x {self.item}"


def apply_discount(order: Order, percent: float) -> float:
    """The order's total with a percentage taken off."""
    return order.total() * (100 - percent) / 100
''',
    # `import os` is unused (ruff F401) and the constant is a type error.
    "shop/orders.py": '''"""Making and pricing orders."""
import os
from shop.models import Order, apply_discount

DEFAULT_QUANTITY: int = "three"


def make_order(item: str, quantity: int) -> Order:
    return Order(item, quantity, 14.0)


def half_price(order: Order) -> float:
    return apply_discount(order, 50)


def clearance(order: Order) -> float:
    return apply_discount(order, 80)


def checkout(order: Order, member: bool) -> float:
    if member:
        return apply_discount(order, 10)
    return order.total()


def label(order: Order) -> str:
    text = ""
    return text
''',
    "main.py": '''"""Runs the sample shop: prints an order's total."""
from shop.models import apply_discount
from shop.orders import make_order


def summarize(count: int) -> float:
    order = make_order("widget", count)
    total = order.total()
    print("order total", total)
    return apply_discount(order, 0)


if __name__ == "__main__":
    value = summarize(3)
    print("done", value)
''',
    "tests/test_orders.py": '''"""Tests of the sample shop; one fails on purpose."""
from shop.models import Order, apply_discount
from shop.orders import make_order


class TestOrders:
    def test_total(self):
        assert make_order("widget", 3).total() == 42.0

    def test_discount_is_wrong(self):
        order = Order("widget", 2, 10.0)
        discounted = apply_discount(order, 50)
        assert discounted == 11.0


def test_describe():
    assert Order("widget", 2, 10.0).describe() == "2 x widget"
''',
}


def main() -> None:
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    folder, venv = os.path.abspath(sys.argv[1]), os.path.abspath(sys.argv[2])
    if not os.path.exists(os.path.join(venv, "bin", "python")):
        sys.exit(f"{venv} has no bin/python")
    shutil.rmtree(folder, ignore_errors=True)
    for name, text in FILES.items():
        path = os.path.join(folder, name)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as file:
            file.write(text)
    os.symlink(venv, os.path.join(folder, ".venv"))
    git = ["git", "-C", folder, "-c", "user.name=selftest", "-c", "user.email=selftest@localhost"]
    subprocess.run(git + ["init", "-q"], check=True)
    subprocess.run(git + ["add", "-A"], check=True)
    subprocess.run(git + ["commit", "-q", "-m", "The sample shop"], check=True)


if __name__ == "__main__":
    main()
