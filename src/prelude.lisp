(define (bash command)
  (kernel/trap :bash command))

(define (bash/start command)
  (kernel/trap :bash-start command))

(define (bash/status job-id)
  (kernel/trap :bash-status job-id))

(define (bash/cancel job-id)
  (kernel/trap :bash-cancel job-id))

(define (bash/collect job-id)
  (kernel/trap :bash-collect job-id))

(define (bash/list)
  (kernel/trap :bash-list nil))

(define (model/call prompt)
  (kernel/trap :model-call prompt))

(define (agent/call name request)
  (kernel/trap :agent-call [name request]))

(define (agent/return value)
  (kernel/trap :agent-return value))

(define (message/reply message-id text)
  (kernel/trap :message-reply [message-id text]))

(define (human/wait)
  (kernel/trap :human-wait nil))

(define (nil? value)
  (= (kernel/type-of value) :nil))

(define (number? value)
  (let ((kind (kernel/type-of value)))
    (if (= kind :integer) #t (= kind :float))))

(define (symbol? value)
  (= (kernel/type-of value) :symbol))

(define (string? value)
  (= (kernel/type-of value) :string))

(define (list? value)
  (let ((kind (kernel/type-of value)))
    (if (= kind :list) #t (= kind :nil))))

(define (function? value)
  (= (kernel/type-of value) :function))

(define (keyword? value)
  (= (kernel/type-of value) :keyword))

(define (not value)
  (if value #f #t))

(define (identity value)
  value)

(define (remember text)
  (memory/note text))
