import logging
import logging.handlers
import queue
import sys
from pathlib import Path
from typing import Optional

def setup_logger(
    name: str, 
    log_file: Optional[Path] = None, 
    level: int = logging.INFO,
    console: bool = True,
    max_bytes: int = 10 * 1024 * 1024,
    backup_count: int = 5
) -> logging.Logger:
    """
    Sets up a non-blocking logger using QueueHandler.
    
    Args:
        name: Logger name.
        log_file: Path to the log file (optional). If provided, file logging is enabled.
        level: Logging level.
        console: Whether to log to console.
    """
    logger = logging.getLogger(name)
    logger.setLevel(level)
    
    # If handlers already exist, clear them to avoid duplicates on reload
    if logger.handlers:
        logger.handlers.clear()

    handlers = []
    
    formatter = logging.Formatter("%(asctime)s %(levelname)s %(message)s")

    # 1. File Handler (if requested)
    if log_file:
        log_file.parent.mkdir(parents=True, exist_ok=True)
        file_handler = logging.handlers.RotatingFileHandler(
            log_file, 
            maxBytes=max_bytes, 
            backupCount=backup_count, 
            encoding="utf-8"
        )
        file_handler.setFormatter(formatter)
        handlers.append(file_handler)

    # 2. Console Handler (if requested)
    if console:
        console_handler = logging.StreamHandler(sys.stdout)
        console_handler.setFormatter(formatter)
        handlers.append(console_handler)

    if not handlers:
        return logger

    # 3. Setup Queue and Listener (The Performance Magic)
    log_queue = queue.Queue(-1) # Infinite queue
    queue_handler = logging.handlers.QueueHandler(log_queue)
    
    logger.addHandler(queue_handler)
    
    # The listener will pop messages from the queue and send them to the actual handlers (File/Stream)
    # respect_handler_level=True ensures handlers' levels are respected
    listener = logging.handlers.QueueListener(log_queue, *handlers, respect_handler_level=True)
    listener.start()
    
    return logger, listener
