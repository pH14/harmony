# Ten seconds of things a computer is never supposed to repeat.
echo "--- a shuffled deck ---"
for s in S H D C; do for r in A 2 3 4 5 6 7 8 9 10 J Q K; do echo "$r$s"; done; done \
    | shuf | tr '\n' ' '; echo
echo "--- five dice ---"
for i in 1 2 3 4 5; do echo -n "$(shuf -i 1-6 -n 1) "; done; echo
echo "--- 16 bytes straight from /dev/urandom ---"
head -c 16 /dev/urandom | od -An -tx1
echo "--- the clock ---"
date; cat /proc/uptime
