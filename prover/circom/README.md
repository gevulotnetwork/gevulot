# Test Circom Circuit

The circom program file used here is from this repository:

https://github.com/nalinbhardwaj/snarky-sudoku

The library of circuits found in the `prover/circom/circuits` fold were taken from the circomlib repo.

https://github.com/iden3/circomlib

There is a readme file there documentating the various circuits.


[CircomLib circuit cibrary readme](circuits/README.md)

## Install `circom` and `snarkjs`

You must have `circom` and `snarkjs`  installed on your system.

1. circom: https://docs.circom.io/getting-started/installation/
```
git clone https://github.com/iden3/circom.git
cd circom
cargo build --release
cargo install --path circom
```
2. snarkjs
```
sudo npm install -g snarkjs
```

## The test circuit

The file is `sudoku.circom`.  This program checks a given solution against a sudoku puzzle.


### Compilation
To compile the script to `r1cs`, `wasm`, and `sym` files, run this command from the `prover/circom` directory.

```
circom sudoku.circom --r1cs --wasm --sym
```

You should see output like this:
```
template instances: 10
non-linear constraints: 4374
linear constraints: 0
public inputs: 81
public outputs: 0
private inputs: 81
private outputs: 0
wires: 3970
labels: 19036
Written successfully: ./sudoku.r1cs
Written successfully: ./sudoku.sym
Written successfully: ./sudoku_js/sudoku.wasm
Everything went okay, circom safe
```
The `r1cs` file will be used directly by us.
The `sym` file is there as a reference, showing the mapping to named variables in the program.
The `wasm` file will be used to generate the witness.

You will also see a newly create folder named `sudoku_js`.

### Input file

The inputs for this puzzle are in `sudoku.json`.

```
{
  "unsolved_grid": [
    [6, 0, 3, 2, 0, 5, 4, 7, 0],
    [7, 9, 1, 4, 6, 0, 0, 2, 8],
    [5, 2, 4, 9, 0, 0, 1, 6, 0],
    [0, 4, 0, 0, 5, 7, 3, 0, 0],
    [3, 1, 0, 6, 8, 0, 7, 4, 2],
    [0, 7, 0, 3, 4, 2, 0, 0, 0],
    [0, 0, 0, 5, 2, 4, 0, 0, 0],
    [1, 0, 0, 7, 3, 0, 0, 0, 4],
    [0, 3, 0, 0, 9, 1, 0, 0, 7]
  ],
  "solved_grid": [
    [6, 8, 3, 2, 1, 5, 4, 7, 9],
    [7, 9, 1, 4, 6, 3, 5, 2, 8],
    [5, 2, 4, 9, 7, 8, 1, 6, 3],
    [2, 4, 9, 1, 5, 7, 3, 8, 6],
    [3, 1, 5, 6, 8, 9, 7, 4, 2],
    [8, 7, 6, 3, 4, 2, 9, 1, 5],
    [9, 6, 7, 5, 2, 4, 8, 3, 1],
    [1, 5, 8, 7, 3, 6, 2, 9, 4],
    [4, 3, 2, 8, 9, 1, 6, 5, 7]
  ]
}
```

### Witness File

Generate the witness file with node and snarkjs.

```
cd sudoku_js
node generate_witness.js sudoku.wasm ../sudoku.json sudoku.wtns
```

If you edit the `sudoku.js` file to make an incorrect solution or unsolved board, the generation of the witness will fail.
