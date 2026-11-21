class Tabularium < Formula
  desc "AI-oriented markdown document store with CLI and HTTP server"
  homepage "https://github.com/velortechnologies/tabularium"
  license "Apache-2.0"
  version "0.2.1"

  on_macos do
    url "https://github.com/velortechnologies/tabularium/releases/download/v0.2.1/tb-v0.2.1-aarch64-apple-darwin.tar.gz"
    sha256 "df74e58db027b94a5e95f9482cb4fcf29fabbb1a45224e5e007189f26112f694"

    resource "tabularium-server-bin" do
      url "https://github.com/velortechnologies/tabularium/releases/download/v0.2.1/tabularium-server-v0.2.1-aarch64-apple-darwin.tar.gz"
      sha256 "d8916f2e7deb8acff36ce74a3d569909580cd94712d4b9029060279e5aa129f3"
    end
  end

  on_linux do
    url "https://github.com/velortechnologies/tabularium/releases/download/v0.2.1/tb-v0.2.1-x86_64-unknown-linux-gnu.tar.gz"
    sha256 "a1b63675ee19a33e46c0dc839f2e32294a277fe1a9e03cee86662034771d59da"

    resource "tabularium-server-bin" do
      url "https://github.com/velortechnologies/tabularium/releases/download/v0.2.1/tabularium-server-v0.2.1-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "07f8dd3bce866e859dc23a3d0809b80de7a22559244148bd7811137783d8d5f7"
    end
  end

  resource "default-config" do
    url "https://raw.githubusercontent.com/velortechnologies/tabularium/v0.2.1/config.toml.example"
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
