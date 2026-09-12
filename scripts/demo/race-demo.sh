# Four workers increment one shared counter. Each write is atomic (rename),
# but read-increment-write is not — concurrent increments get lost. On a
# normal machine the loss count jitters run to run; here it replays exactly.
echo 0 > /tmp/counter
for w in 1 2 3 4; do
    (
        i=0
        while [ $i -lt 2000 ]; do
            n=$(cat /tmp/counter)
            echo $((n + 1)) > /tmp/counter.$w
            mv /tmp/counter.$w /tmp/counter
            i=$((i + 1))
        done
    ) &
done
wait
final=$(cat /tmp/counter)
echo "expected  8000 increments"
echo "observed  $final"
echo "lost      $((8000 - final)) updates to the race"
