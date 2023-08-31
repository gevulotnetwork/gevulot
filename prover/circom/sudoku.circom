pragma circom 2.0.0;

include "circuits/comparators.circom";
include "circuits/gates.circom";

template InRange(bits) {
  signal input value;
  signal input lower;
  signal input upper;
  signal output out;

  component upperBound = LessEqThan(bits);
  upperBound.in[0] <== value;
  upperBound.in[1] <== upper;

  component lowerBound = GreaterEqThan(bits);
  lowerBound.in[0] <== value;
  lowerBound.in[1] <== lower;

  out <== upperBound.out*lowerBound.out;
}

template ContainsAll(SIZE) {
  signal input in[SIZE];

  component is_equal[SIZE][SIZE];
  for (var i = 0;i < SIZE;i++) {
    for (var j = 0;j < SIZE;j++) {
      is_equal[i][j] = IsEqual();
    }
  }

  for (var i = 0;i < SIZE;i++) {
    for (var j = 0;j < SIZE;j++) {
      is_equal[i][j].in[0] <== in[i];
      is_equal[i][j].in[1] <== (i == j) ? 0 : in[j];
      is_equal[i][j].out === 0;
    }
  }
}

template Main(SIZE, SUBSIZE) {
  signal input unsolved_grid[SIZE][SIZE];
  signal input solved_grid[SIZE][SIZE];

  component range_checkers[SIZE][SIZE][2];
  component is_solution[SIZE][SIZE];
  component is_equal[SIZE][SIZE];
  component is_valid[SIZE][SIZE];
  component row_checkers[SIZE];
  component col_checkers[SIZE];
  component submat_checkers[SIZE];
  for (var i = 0;i < SIZE;i++) {
    row_checkers[i] = ContainsAll(SIZE);
    col_checkers[i] = ContainsAll(SIZE);
    submat_checkers[i] = ContainsAll(SIZE);
    for (var j = 0;j < SIZE;j++) {
      for (var k = 0;k < 2;k++) {
        range_checkers[i][j][k] = InRange(4);
      }

      is_solution[i][j] = IsZero();
      is_equal[i][j] = IsEqual();
      is_valid[i][j] = OR();
    }
  }

  // Assert input grids are valid and solved grid matches unsolved grid
  for (var i = 0;i < SIZE;i++) {
    for (var j = 0;j < SIZE;j++) {
      range_checkers[i][j][0].value <== solved_grid[i][j];
      range_checkers[i][j][0].upper <== SIZE;
      range_checkers[i][j][0].lower <== 1;
      range_checkers[i][j][0].out === 1;

      range_checkers[i][j][1].value <== unsolved_grid[i][j];
      range_checkers[i][j][1].upper <== SIZE;
      range_checkers[i][j][1].lower <== 0;
      range_checkers[i][j][1].out === 1;

      is_solution[i][j].in <== unsolved_grid[i][j];

      is_equal[i][j].in[0] <== solved_grid[i][j];
      is_equal[i][j].in[1] <== unsolved_grid[i][j];

      is_valid[i][j].a <== is_equal[i][j].out;
      is_valid[i][j].b <== is_solution[i][j].out;
      is_valid[i][j].out === 1;
    }
  }

  // Check rows
  for (var i = 0;i < SIZE;i++) {
    for (var j = 0;j < SIZE;j++) {
      row_checkers[i].in[j] <== solved_grid[i][j];
    }
  }

  // Check columns
  for (var i = 0;i < SIZE;i++) {
    for (var j = 0;j < SIZE;j++) {
      col_checkers[i].in[j] <== solved_grid[j][i];
    }
  }

  // Check submatrices
  for (var i = 0;i < SIZE;i += SUBSIZE) {
    for (var j = 0;j < SIZE;j += SUBSIZE) {
      for (var k = 0;k < SUBSIZE;k++) {
        for (var l = 0;l < SUBSIZE;l++) {
          submat_checkers[i + j / SUBSIZE].in[k*SUBSIZE + l] <== solved_grid[i + k][j + l];
        }
      }
    }
  }
}

component main {public [unsolved_grid]} = Main(9, 3);
