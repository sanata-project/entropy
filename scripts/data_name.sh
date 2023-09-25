num=0
until [[ ! -f ./data/$1-${num}.txt ]]; do
num=$((num +1))
done
echo ./data/$1-${num}.txt