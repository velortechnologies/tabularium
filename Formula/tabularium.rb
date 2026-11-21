class Tabularium < Formula
  desc "AI-oriented markdown document store with CLI and HTTP server"
  homepage "https://github.com/velortechnologies/tabularium"
  license "Apache-2.0"
  version "0.2.1"

  on_macos do
    url "https://github.com/velortechnologies/tabularium/releases/download/v0.2.1/tb-v0.2.1-aarch64-apple-darwin.tar.gz"
    sha256 "756abfd76aabdb4f6aa4f06e7314f752be6434ef85c45141faeee3ca909a1579"

    resource "tabularium-server-bin" do
      url "https://github.com/velortechnologies/tabularium/releases/download/v0.2.1/tabularium-server-v0.2.1-aarch64-apple-darwin.tar.gz"
      sha256 "c4ae02b1d0089974d6961f4899ab0057500fd754d9a8a57f95effc172c7c895c"
    end
  end

  on_linux do
    url "https://github.com/velortechnologies/tabularium/releases/download/v0.2.1/tb-v0.2.1-x86_64-unknown-linux-gnu.tar.gz"
    sha256 "559c55921fe6c1def57e4ce356150f43670bc4382a4214bd12a87bbb4d2cec06"

    resource "tabularium-server-bin" do
      url "https://github.com/velortechnologies/tabularium/releases/download/v0.2.1/tabularium-server-v0.2.1-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "0f698f591dd203027c82d48836b9b64d2d527ad0e76e88a02fcb86442c973ce8"
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
