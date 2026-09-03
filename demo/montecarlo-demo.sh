# Monte Carlo estimation of pi in Python, seeded from the kernel's entropy
# pool like any real workload. The estimate depends on two million random
# draws — and lands on the same digits every run.
python3 - <<'PY'
import random, uuid
print("three fresh uuid4s:", *(uuid.uuid4() for _ in range(3)), sep="\n  ")
n = 2_000_000
hits = sum(random.random() ** 2 + random.random() ** 2 <= 1.0 for _ in range(n))
pi = 4 * hits / n
print(f"samples   {n}")
print(f"pi        {pi:.6f}")
print(f"error     {abs(pi - 3.141592653589793):.6f}")
PY
