import argparse
import subprocess
import time
import os
import signal
import sys
from pathlib import Path
from typing import List

# Configuration
PROJECT_ROOT = Path(__file__).parent
# Binary name is platform-specific: Windows appends `.exe`, Linux/macOS do not.
RUST_BIN_NAME = "rust_trader.exe" if os.name == "nt" else "rust_trader"
RUST_BIN_PATH = PROJECT_ROOT / "engine_rust" / "target" / "release" / RUST_BIN_NAME
PYTHON_SCRIPT = PROJECT_ROOT / "engine_python.py"

processes: List[subprocess.Popen] = []

def signal_handler(sig, frame):
    print("\n🛑 Shutting down all bots...")
    for p in processes:
        p.terminate()
    sys.exit(0)

signal.signal(signal.SIGINT, signal_handler)

def run_python_bot():
    print("🐍 Starting Python Engine...")
    p = subprocess.Popen([sys.executable, str(PYTHON_SCRIPT)])
    processes.append(p)

def run_rust_bot():
    if not RUST_BIN_PATH.exists():
        print(f"❌ Rust binary not found at: {RUST_BIN_PATH}")
        print("💡 Please run: cd engine_rust && cargo build --release")
        return
    
    print("🦀 Starting Rust Engine...")
    # Rust reads .env from current dir usually, so we run from project root or pass env?
    # dotenv in Rust looks for .env in current dir or parents.
    p = subprocess.Popen([str(RUST_BIN_PATH)], cwd=str(PROJECT_ROOT))
    processes.append(p)

def main():
    parser = argparse.ArgumentParser(description="Trading Bot Unified Launcher 🚀")
    parser.add_argument("--mode", choices=["python", "rust", "duel"], default="python", help="Execution mode")
    
    args = parser.parse_args()
    
    print(f"🔥 Launching in {args.mode.upper()} mode")
    print("="*40)

    if args.mode == "python":
        run_python_bot()
    elif args.mode == "rust":
        run_rust_bot()
    elif args.mode == "duel":
        print("⚔️ DUEL MODE: Starting both engines!")
        run_python_bot()
        time.sleep(1) # Small delay to avoid log mix-up on start
        run_rust_bot()

    # Keep alive
    try:
        while True:
            time.sleep(1)
            # Check if processes are alive
            for p in processes:
                if p.poll() is not None:
                    print(f"⚠️ Process {p.pid} exited unexpectedly!")
                    processes.remove(p)
            
            if not processes:
                print("All processes finished. Exiting.")
                break
    except KeyboardInterrupt:
        pass # Handled by signal_handler

if __name__ == "__main__":
    main()
