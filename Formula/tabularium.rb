class Tabularium < Formula
  desc "AI-oriented markdown document store with CLI and HTTP server"
  homepage "https://github.com/velortechnologies/tabularium"
  license "Apache-2.0"
  version "0.2.0"

  on_macos do
    url "https://github.com/velortechnologies/tabularium/releases/download/v0.2.0/tb-v0.2.0-aarch64-apple-darwin.tar.gz"
    sha256 "02f4d26b9c9d6e6c789c4b3bc2e2b44e0eb831812790614642eb068de0346abc"

    resource "tabularium-server-bin" do
      url "https://github.com/velortechnologies/tabularium/releases/download/v0.2.0/tabularium-server-v0.2.0-aarch64-apple-darwin.tar.gz"
      sha256 "dac6d0c3615666b30c8ac350f9041fef6fe1c81388052405d9975cd986a1e3a6"
    end
  end

  on_linux do
    url "https://github.com/velortechnologies/tabularium/releases/download/v0.2.0/tb-v0.2.0-x86_64-unknown-linux-gnu.tar.gz"
    sha256 "91998b0e7c7c46e79283d9c4fe533d91f53aae2367527689bee209e523a705ae"

    resource "tabularium-server-bin" do
      url "https://github.com/velortechnologies/tabularium/releases/download/v0.2.0/tabularium-server-v0.2.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "f0a944a6a1e43d552c37c0c250d0216bd40c5aee11fc9b8a72b33e20b3816df5"
    end
  end

  resource "default-config" do
    url "https://raw.githubusercontent.com/velortechnologies/tabularium/v0.2.0/config.toml.example"
    sha256 "80f9a69158962b7423015fd956f2036dcb82de36ac4f3272f6c11063488631a4"
  end

  def install
    bin.install "tb"
    resource("tabularium-server-bin").stage do
      bin.install "tabularium-server"
    end
    (etc/"tabularium").mkpath
    unless (etc/"tabularium/config.toml").exist?
      resource("default-config").stage do
        cp "config.toml.example", etc/"tabularium/config.toml"
      end
      inreplace etc/"tabularium/config.toml" do |s|
        s.gsub!("./data/tabularium.db", "#{var}/tabularium/tabularium.db")
        s.gsub!("./data/tabularium.index", "#{var}/tabularium/tabularium.index")
      end
    end
  end

  def post_install
    (var/"tabularium").mkpath
  end

  service do
    run [opt_bin/"tabularium-server", "--config", etc/"tabularium/config.toml"]
    keep_alive true
    working_dir var/"tabularium"
    log_path var/"log/tabularium.log"
    error_log_path var/"log/tabularium.error.log"
  end

  test do
    assert_predicate bin/"tb", :exist?
    assert_predicate bin/"tabularium-server", :exist?
  end
end
