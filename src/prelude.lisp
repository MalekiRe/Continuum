(define (bash command)
  (kernel/trap :bash command))

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
