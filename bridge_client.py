import socket
import json
import argparse
import time

# Configuration (Fill this on Europe Server)
TOKYO_IP = "127.0.0.1" # Replace with Tokyo EC2 Public IP
TOKYO_PORT = 5005

def send_udp_signal(symbol: str, side: str):
    payload = {
        "symbol": symbol,
        "side": side,
        "source": "bridge_eu"
    }
    msg_bytes = json.dumps(payload).encode("utf-8")
    
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    
    print(f"📡 Sending UDP to {TOKYO_IP}:{TOKYO_PORT} -> {payload}")
    
    # Redundancy: Send 3 times
    for i in range(3):
        sock.sendto(msg_bytes, (TOKYO_IP, TOKYO_PORT))
        # No sleep between redundant packets for max speed
        
    sock.close()

if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("symbol", help="e.g. BTCUSDT")
    parser.add_argument("side", choices=["BUY", "SELL"])
    parser.add_argument("--ip", help="Override Target IP", default=TOKYO_IP)
    
    args = parser.parse_args()
    
    if args.ip:
        TOKYO_IP = args.ip
        
    send_udp_signal(args.symbol, args.side)
