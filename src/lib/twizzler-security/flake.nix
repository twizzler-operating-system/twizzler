{
  description = "gnu plot flake";

  outputs = {
    self,
    nixpkgs,
  }: let
    pkgs = import nixpkgs {
      system = "x86_64-linux";
    };
  in {
    packages.x86_64-linux.gnuplot = pkgs.gnuplot;

    defaultPackage.x86_64-linux = self.packages.x86_64-linux.gnuplot;

    devShells.x86_64-linux.default = pkgs.mkShell {
      name = "gnuplot-shell";
      buildInputs = [
        pkgs.gnuplot
      ];
    };
  };
}
